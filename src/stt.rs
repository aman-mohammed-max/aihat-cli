//! Phonon-2 speech recognition, all in Rust and on this computer.
//!
//! Phonon-2 (FermionResearch, CC-BY-4.0) is NVIDIA's parakeet-tdt-0.6b-v3
//! stored in 164 MB: a FastConformer encoder (8× convolution subsampling,
//! 24 conformer blocks with relative-position attention) and a TDT
//! transducer (an LSTM prediction network and a joint network that picks a
//! token and how many frames to move on). This follows Hugging Face's
//! `ParakeetForTDT`. Weights stay in their compact form (8-bit, or
//! "five-value": 0, ±lo, ±hi per row) and are expanded one layer at a time.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::Path;

const D: usize = 1024; // encoder width
const HEADS: usize = 8;
const HEAD: usize = D / HEADS;
const LAYERS: usize = 24;
const H: usize = 640; // prediction / joint width
const VOCAB: usize = 8193; // tokens, the last one blank
const BLANK: usize = 8192;
const DURATIONS: [usize; 5] = [0, 1, 2, 3, 4];
const MELS: usize = 128;
const SUB: usize = 256; // subsampling channels

// ------------------------------------------------------------------ weights

fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) as u32) << 31;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x3ff) as u32;
    let value = match exp {
        0 if frac == 0 => sign,
        0 => {
            // Subnormal: normalise it.
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((f & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(value)
}

fn halves(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(2).map(|b| f16_to_f32(u16::from_le_bytes([b[0], b[1]]))).collect()
}

/// A weight, kept as packed in the file (about 230 MB for the whole model)
/// and unpacked a row at a time: plain floats; 6- or 8-bit integers with a
/// scale per row; or five-value codes (base-3 digits, five to a byte, and a
/// bit for each non-zero choosing lo or hi).
enum Store {
    F32(Vec<f32>),
    Int { packed: Vec<u8>, bits: u32, scale: Vec<f32> },
    Five { trits: Vec<u8>, starts: Vec<usize>, bits: Vec<u8>, lo: Vec<f32>, hi: Vec<f32> },
}

pub struct Tensor {
    shape: Vec<usize>,
    store: Store,
}

impl Tensor {
    fn rows(&self) -> usize {
        self.shape[0]
    }

    fn cols(&self) -> usize {
        self.shape[1..].iter().product::<usize>().max(1)
    }

    /// The weights as floats, written into `out` (reused between layers),
    /// rows spread over the CPU's cores.
    fn expand_into(&self, out: &mut Vec<f32>) {
        let cols = self.cols();
        let total = self.rows() * cols;
        out.clear();
        if let Store::F32(data) = &self.store {
            out.extend_from_slice(data);
            return;
        }
        out.resize(total, 0.0);
        par_rows(out, cols, |row, dest| match &self.store {
            Store::Int { packed, bits, scale } => {
                let s = scale[row];
                for (j, d) in dest.iter_mut().enumerate() {
                    let index = row * cols + j;
                    let q = if *bits == 8 {
                        packed[index] as i8 as i32
                    } else {
                        let g = index / 4 * 3;
                        let word = packed[g] as u32 | (packed[g + 1] as u32) << 8 | (packed[g + 2] as u32) << 16;
                        ((word >> (6 * (index % 4))) & 0x3f) as i32 - 32
                    };
                    *d = q as f32 * s;
                }
            }
            Store::Five { trits, starts, bits, lo, hi } => {
                let (l, h) = (lo[row], hi[row]);
                let rb = cols.div_ceil(5);
                let table = trit_table();
                let mut n = starts[row];
                for (byte, &b) in trits[row * rb..(row + 1) * rb].iter().enumerate() {
                    for (k, &digit) in table[b as usize].iter().enumerate() {
                        let Some(d) = dest.get_mut(byte * 5 + k) else { break };
                        *d = if digit == 0 {
                            0.0
                        } else {
                            let high = bits[n / 8] >> (n % 8) & 1 == 1;
                            n += 1;
                            digit as f32 * if high { h } else { l }
                        };
                    }
                }
            }
            Store::F32(_) => {}
        });
    }

    fn floats(&self) -> Vec<f32> {
        let mut out = Vec::new();
        self.expand_into(&mut out);
        out
    }
}

/// Each byte holds five base-3 digits (0, 1, 2 meaning −, 0, +).
fn trit_table() -> &'static [[i8; 5]; 256] {
    static TABLE: std::sync::OnceLock<[[i8; 5]; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [[0i8; 5]; 256];
        for (byte, digits) in table.iter_mut().enumerate() {
            let mut x = byte;
            for d in digits.iter_mut() {
                *d = (x % 3) as i8 - 1;
                x /= 3;
            }
        }
        table
    })
}

/// Run `f(row, values)` for every row of `data` (rows of `cols`), on all cores.
fn par_rows<T: Send>(data: &mut [T], cols: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let rows = data.len() / cols.max(1);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(rows.max(1));
    let per = rows.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|scope| {
        for (chunk, part) in data.chunks_mut(per * cols).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (i, row) in part.chunks_mut(cols).enumerate() {
                    f(chunk * per + i, row);
                }
            });
        }
    });
}

fn five_value(blob: &[u8], o: usize, i: usize) -> Result<Store> {
    let rb = i.div_ceil(5);
    let trits = blob.get(..o * rb).context("short five-value record")?.to_vec();
    let table = trit_table();
    // Where each row's lo/hi bits start: the non-zeros before it.
    let mut starts = Vec::with_capacity(o);
    let mut nonzero = 0usize;
    for row in 0..o {
        starts.push(nonzero);
        for (byte, &b) in trits[row * rb..(row + 1) * rb].iter().enumerate() {
            nonzero += table[b as usize].iter().take(i.saturating_sub(byte * 5)).filter(|&&d| d != 0).count();
        }
    }
    let at = o * rb;
    let bits = blob.get(at..at + nonzero.div_ceil(8)).context("short five-value record")?.to_vec();
    let at = at + nonzero.div_ceil(8);
    let lo = halves(blob.get(at..at + 2 * o).context("short five-value record")?);
    let hi = halves(blob.get(at + 2 * o..at + 4 * o).context("short five-value record")?);
    if at + 4 * o != blob.len() {
        bail!("five-value record has {} bytes, expected {}", blob.len(), at + 4 * o);
    }
    Ok(Store::Five { trits, starts, bits, lo, hi })
}

fn int_n(blob: &[u8], shape: &[usize], bits: u32) -> Result<Store> {
    let o = shape[0];
    let total: usize = shape.iter().product();
    let (body, scales) = blob.split_at(blob.len().checked_sub(2 * o).context("short int record")?);
    let needed = match bits {
        8 => total,
        6 => total.div_ceil(4) * 3,
        other => bail!("int{other} weights aren't supported"),
    };
    if body.len() < needed {
        bail!("int record is short");
    }
    Ok(Store::Int { packed: body[..needed].to_vec(), bits, scale: halves(scales) })
}

/// Read `model.fermion` (format fermion-five-value-parakeet-v1).
pub fn read_container(path: &Path) -> Result<HashMap<String, Tensor>> {
    let mut file = BufReader::new(std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?);
    let mut len = [0u8; 8];
    file.read_exact(&mut len)?;
    let mut header = vec![0u8; u64::from_le_bytes(len) as usize];
    file.read_exact(&mut header)?;
    let header: Value = serde_json::from_slice(&header).context("reading the model's index")?;
    if header.get("format").and_then(Value::as_str) != Some("fermion-five-value-parakeet-v1") {
        bail!("this isn't a Phonon-2 model file");
    }
    let mut tensors = HashMap::new();
    for entry in header.get("index").and_then(Value::as_array).context("the model has no index")? {
        let name = entry.get("n").and_then(Value::as_str).context("unnamed record")?.to_string();
        let kind = entry.get("k").and_then(Value::as_str).unwrap_or("");
        let shape: Vec<usize> = entry.get("shape").and_then(Value::as_array).map(|s| s.iter().filter_map(Value::as_u64).map(|d| d as usize).collect()).unwrap_or_default();
        let size = entry.get("b").and_then(Value::as_u64).context("record without a size")? as usize;
        let mut blob = vec![0u8; size];
        file.read_exact(&mut blob)?;
        let (name, store) = match kind {
            "five_value" => (format!("{name}.weight"), five_value(&blob, shape[0], shape[1..].iter().product())?),
            "fp16" => (name, Store::F32(halves(&blob))),
            k if k.starts_with("int") => (name, int_n(&blob, &shape, k[3..].parse().map_err(|_| anyhow!("unknown record kind {k}"))?)?),
            other => bail!("unknown record kind {other}"),
        };
        tensors.insert(name, Tensor { shape: if shape.is_empty() { vec![1] } else { shape }, store });
    }
    Ok(tensors)
}

// ------------------------------------------------------------------ maths

/// y[t×n] = x[t×k] · Wᵀ (W is n×k), plus a bias.
fn matmul_t(x: &[f32], t: usize, w: &[f32], n: usize, k: usize, bias: Option<&[f32]>) -> Vec<f32> {
    let mut y = vec![0f32; t * n];
    if let Some(bias) = bias {
        for row in y.chunks_exact_mut(n) {
            row.copy_from_slice(bias);
        }
    }
    // Worked as yᵀ = W · xᵀ: the many weight rows split well across cores,
    // where the few time steps don't. SAFETY: x is t×k, w is n×k and y is
    // t×n, matching the strides given.
    unsafe {
        matrixmultiply::sgemm(n, k, t, 1.0, w.as_ptr(), k as isize, 1, x.as_ptr(), 1, k as isize, if bias.is_some() { 1.0 } else { 0.0 }, y.as_mut_ptr(), 1, n as isize);
    }
    y
}

/// y = W·x + b for one vector, rows spread over the cores.
fn matvec(w: &[f32], x: &[f32], bias: &[f32]) -> Vec<f32> {
    let mut y = bias.to_vec();
    let k = x.len();
    let rows = y.len();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let per = rows.div_ceil(threads).max(64);
    std::thread::scope(|scope| {
        for (chunk, part) in y.chunks_mut(per).enumerate() {
            scope.spawn(move || {
                for (i, out) in part.iter_mut().enumerate() {
                    let row = &w[(chunk * per + i) * k..(chunk * per + i + 1) * k];
                    *out += row.iter().zip(x).map(|(a, b)| a * b).sum::<f32>();
                }
            });
        }
    });
    y
}

fn silu(v: f32) -> f32 {
    v / (1.0 + (-v).exp())
}

fn sigmoid(v: f32) -> f32 {
    1.0 / (1.0 + (-v).exp())
}

fn layer_norm(x: &[f32], width: usize, weight: &[f32], bias: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(x.len());
    for row in x.chunks_exact(width) {
        let mean = row.iter().sum::<f32>() / width as f32;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / width as f32;
        let inv = 1.0 / (var + 1e-5).sqrt();
        out.extend(row.iter().enumerate().map(|(i, v)| (v - mean) * inv * weight[i] + bias[i]));
    }
    out
}

// ------------------------------------------------------------------ audio

/// Samples at 16 kHz, mono, from a WAV or FLAC file.
pub fn read_audio(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let (samples, rate, channels) = if bytes.starts_with(b"fLaC") {
        let mut reader = claxon::FlacReader::new(std::io::Cursor::new(&bytes)).context("reading the FLAC file")?;
        let info = reader.streaminfo();
        let scale = 1.0 / (1u64 << (info.bits_per_sample - 1)) as f32;
        let samples: Vec<f32> = reader.samples().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>().context("reading the FLAC file")?;
        (samples, info.sample_rate, info.channels as usize)
    } else {
        read_wav(&bytes)?
    };
    let mono: Vec<f32> = if channels > 1 { samples.chunks(channels).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect() } else { samples };
    Ok(if rate == 16_000 { mono } else { resample(&mono, rate, 16_000) })
}

fn read_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32, usize)> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("only WAV and FLAC audio can be read");
    }
    let (mut format, mut channels, mut rate, mut bits, mut data) = (1u16, 1usize, 16_000u32, 16u16, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            format = u16::from_le_bytes([body[0], body[1]]);
            channels = u16::from_le_bytes([body[2], body[3]]) as usize;
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
            bits = u16::from_le_bytes([body[14], body[15]]);
            if format == 0xfffe && body.len() >= 26 {
                format = u16::from_le_bytes([body[24], body[25]]);
            }
        } else if id == b"data" {
            data = Some(body);
        }
        at += 8 + size + (size & 1);
    }
    let data = data.context("the WAV file has no sound in it")?;
    let samples = match (format, bits) {
        (1, 16) => data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect(),
        (1, 24) => data.chunks_exact(3).map(|b| ((i32::from_le_bytes([0, b[0], b[1], b[2]])) >> 8) as f32 / 8_388_608.0).collect(),
        (1, 32) => data.chunks_exact(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2_147_483_648.0).collect(),
        (1, 8) => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        (3, 32) => data.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect(),
        _ => bail!("this WAV encoding (format {format}, {bits} bits) can't be read"),
    };
    Ok((samples, rate, channels.max(1)))
}

/// Linear resampling: plenty for speech at 16 kHz.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    let ratio = from as f64 / to as f64;
    let len = (input.len() as f64 / ratio) as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let a = input.get(j).copied().unwrap_or(0.0);
            let b = input.get(j + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * std::f64::consts::PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (angle * k as f64).sin_cos();
                let (wr, wi) = (c as f32, s as f32);
                let (a, b) = (start + k, start + k + len / 2);
                let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        len <<= 1;
    }
}

fn hz_to_mel(f: f64) -> f64 {
    let sp = 200.0 / 3.0;
    if f >= 1000.0 { 1000.0 / sp + (f / 1000.0).ln() / (6.4f64.ln() / 27.0) } else { f / sp }
}

fn mel_to_hz(m: f64) -> f64 {
    let sp = 200.0 / 3.0;
    let min_log_mel = 1000.0 / sp;
    if m >= min_log_mel { 1000.0 * ((6.4f64.ln() / 27.0) * (m - min_log_mel)).exp() } else { m * sp }
}

/// librosa's Slaney mel filters: 128 × 257.
fn mel_filters() -> Vec<f32> {
    let bins = 257;
    let fft_freqs: Vec<f64> = (0..bins).map(|k| k as f64 * 16_000.0 / 512.0).collect();
    let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(8000.0));
    let mel_f: Vec<f64> = (0..MELS + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (MELS + 1) as f64)).collect();
    let mut weights = vec![0f32; MELS * bins];
    for m in 0..MELS {
        let enorm = 2.0 / (mel_f[m + 2] - mel_f[m]);
        for (k, &f) in fft_freqs.iter().enumerate() {
            let lower = (f - mel_f[m]) / (mel_f[m + 1] - mel_f[m]);
            let upper = (mel_f[m + 2] - f) / (mel_f[m + 2] - mel_f[m + 1]);
            weights[m * bins + k] = (lower.min(upper).max(0.0) * enorm) as f32;
        }
    }
    weights
}

/// Normalised log-mel features, frames × 128, as ParakeetFeatureExtractor.
pub fn features(audio: &[f32]) -> (Vec<f32>, usize) {
    let mut x = Vec::with_capacity(audio.len());
    for (i, &s) in audio.iter().enumerate() {
        x.push(if i == 0 { s } else { s - 0.97 * audio[i - 1] });
    }
    // A Hann window of 400 inside 512, frames centred (256 zeros each side).
    let window: Vec<f32> = (0..400).map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / 399.0).cos()).collect();
    let frames = audio.len() / 160;
    let filters = mel_filters();
    let mut out = vec![0f32; frames * MELS];
    let (mut re, mut im) = (vec![0f32; 512], vec![0f32; 512]);
    for t in 0..frames {
        re.iter_mut().for_each(|v| *v = 0.0);
        im.iter_mut().for_each(|v| *v = 0.0);
        for n in 0..400 {
            let idx = (t * 160 + 56 + n) as isize - 256;
            if idx >= 0 && (idx as usize) < x.len() {
                re[56 + n] = x[idx as usize] * window[n];
            }
        }
        fft(&mut re, &mut im);
        let power: Vec<f32> = (0..257).map(|k| re[k] * re[k] + im[k] * im[k]).collect();
        for m in 0..MELS {
            let row = &filters[m * 257..(m + 1) * 257];
            let energy: f32 = row.iter().zip(&power).map(|(w, p)| w * p).sum();
            out[t * MELS + m] = (energy + 2f32.powi(-24)).ln();
        }
    }
    // Each mel band to zero mean and unit spread over the frames.
    if frames > 1 {
        for m in 0..MELS {
            let mean = (0..frames).map(|t| out[t * MELS + m]).sum::<f32>() / frames as f32;
            let var = (0..frames).map(|t| (out[t * MELS + m] - mean).powi(2)).sum::<f32>() / (frames - 1) as f32;
            let std = var.sqrt() + 1e-5;
            for t in 0..frames {
                out[t * MELS + m] = (out[t * MELS + m] - mean) / std;
            }
        }
    }
    (out, frames)
}

// ------------------------------------------------------------------ model

pub struct Phonon {
    t: HashMap<String, Tensor>,
    vocab: Vec<String>,
    special: Vec<bool>,
    // Kept expanded: the parts used once per decoding step.
    embedding: Vec<f32>,
    lstm: Vec<(Vec<f32>, Vec<f32>, Vec<f32>)>,
    dec_proj: (Vec<f32>, Vec<f32>),
    head: (Vec<f32>, Vec<f32>),
}

impl Phonon {
    /// Load `model.fermion` and `tokenizer.json` from a folder.
    pub fn load(dir: &Path) -> Result<Self> {
        let started = std::time::Instant::now();
        let t = read_container(&dir.join("model.fermion"))?;
        debug_time("reading the model", started);
        let (vocab, special) = read_vocab(&dir.join("tokenizer.json"))?;
        let get = |name: &str| t.get(name).map(Tensor::floats).ok_or_else(|| anyhow!("the model has no {name}"));
        let mut lstm = Vec::new();
        for layer in 0..2 {
            let w_ih = get(&format!("decoder.lstm.weight_ih_l{layer}"))?;
            let w_hh = get(&format!("decoder.lstm.weight_hh_l{layer}"))?;
            let bias: Vec<f32> = get(&format!("decoder.lstm.bias_ih_l{layer}"))?.iter().zip(get(&format!("decoder.lstm.bias_hh_l{layer}"))?).map(|(a, b)| a + b).collect();
            lstm.push((w_ih, w_hh, bias));
        }
        Ok(Self {
            embedding: get("decoder.embedding.weight")?,
            dec_proj: (get("decoder.decoder_projector.weight")?, get("decoder.decoder_projector.bias")?),
            head: (get("joint.head.weight")?, get("joint.head.bias")?),
            lstm,
            vocab,
            special,
            t,
        })
    }

    fn w(&self, name: &str) -> Result<&Tensor> {
        self.t.get(name).ok_or_else(|| anyhow!("the model has no {name}"))
    }

    fn floats(&self, name: &str) -> Result<Vec<f32>> {
        Ok(self.w(name)?.floats())
    }

    /// x (rows × in) through the linear layer `name` (weight, optional bias).
    fn linear(&self, x: &[f32], rows: usize, name: &str, buf: &mut Vec<f32>) -> Result<Vec<f32>> {
        let w = self.w(&format!("{name}.weight"))?;
        w.expand_into(buf);
        let bias = self.t.get(&format!("{name}.bias")).map(Tensor::floats);
        Ok(matmul_t(x, rows, buf, w.rows(), w.cols(), bias.as_deref()))
    }

    fn norm(&self, x: &[f32], name: &str) -> Result<Vec<f32>> {
        Ok(layer_norm(x, D, &self.floats(&format!("{name}.weight"))?, &self.floats(&format!("{name}.bias"))?))
    }

    /// Mel features (frames × 128) to encoder output (steps × 640, projected).
    fn encode(&self, mel: &[f32], frames: usize) -> Result<(Vec<f32>, usize)> {
        let started = std::time::Instant::now();
        let mut buf = Vec::new();
        // 8× subsampling: a 3×3 stride-2 convolution, then twice a depthwise
        // 3×3 stride-2 and a pointwise 1×1, each followed by ReLU.
        let conv0_w = self.floats("encoder.subsampling.layers.0.weight")?;
        let conv0_b = self.floats("encoder.subsampling.layers.0.bias")?;
        let (mut h, mut w) = (frames.saturating_sub(1) / 2 + 1, MELS / 2);
        let mut x = vec![0f32; SUB * h * w];
        for c in 0..SUB {
            for ty in 0..h {
                for fx in 0..w {
                    let mut s = conv0_b[c];
                    for ky in 0..3 {
                        let yy = (2 * ty + ky) as isize - 1;
                        if yy < 0 || yy as usize >= frames {
                            continue;
                        }
                        for kx in 0..3 {
                            let xx = (2 * fx + kx) as isize - 1;
                            if xx < 0 || xx as usize >= MELS {
                                continue;
                            }
                            s += conv0_w[c * 9 + ky * 3 + kx] * mel[yy as usize * MELS + xx as usize];
                        }
                    }
                    x[(c * h + ty) * w + fx] = s.max(0.0);
                }
            }
        }
        for (dw, pw) in [(2, 3), (5, 6)] {
            let dw_w = self.floats(&format!("encoder.subsampling.layers.{dw}.weight"))?;
            let dw_b = self.floats(&format!("encoder.subsampling.layers.{dw}.bias"))?;
            let (nh, nw) = (h.saturating_sub(1) / 2 + 1, w / 2);
            let mut y = vec![0f32; SUB * nh * nw];
            for c in 0..SUB {
                for ty in 0..nh {
                    for fx in 0..nw {
                        let mut s = dw_b[c];
                        for ky in 0..3 {
                            let yy = (2 * ty + ky) as isize - 1;
                            if yy < 0 || yy as usize >= h {
                                continue;
                            }
                            for kx in 0..3 {
                                let xx = (2 * fx + kx) as isize - 1;
                                if xx < 0 || xx as usize >= w {
                                    continue;
                                }
                                s += dw_w[c * 9 + ky * 3 + kx] * x[(c * h + yy as usize) * w + xx as usize];
                            }
                        }
                        y[(c * nh + ty) * nw + fx] = s;
                    }
                }
            }
            // Pointwise: (positions × 256) · Wᵀ, then back to channels first.
            let positions = nh * nw;
            let mut by_pos = vec![0f32; positions * SUB];
            for c in 0..SUB {
                for p in 0..positions {
                    by_pos[p * SUB + c] = y[c * positions + p];
                }
            }
            let out = self.linear(&by_pos, positions, &format!("encoder.subsampling.layers.{pw}"), &mut buf)?;
            x = vec![0f32; SUB * positions];
            for p in 0..positions {
                for c in 0..SUB {
                    x[c * positions + p] = out[p * SUB + c].max(0.0);
                }
            }
            (h, w) = (nh, nw);
        }
        let steps = h;
        // Each step's channels × frequencies, flattened channel first.
        let mut flat = vec![0f32; steps * SUB * w];
        for t in 0..steps {
            for c in 0..SUB {
                for f in 0..w {
                    flat[t * SUB * w + c * w + f] = x[(c * h + t) * w + f];
                }
            }
        }
        let mut hs = self.linear(&flat, steps, "encoder.subsampling.linear", &mut buf)?;
        debug_time("subsampling", started);

        // Relative positions from steps−1 down to −(steps−1), as sin/cos pairs.
        let span = 2 * steps - 1;
        let mut pos = vec![0f32; span * D];
        for k in 0..span {
            let p = (steps as f32 - 1.0) - k as f32;
            for m in 0..D / 2 {
                let freq = 1.0 / 10000f32.powf((2 * m) as f32 / D as f32);
                pos[k * D + 2 * m] = (p * freq).sin();
                pos[k * D + 2 * m + 1] = (p * freq).cos();
            }
        }

        for layer in 0..LAYERS {
            let p = format!("encoder.layers.{layer}");
            // Half a feed-forward, attention, convolution, half a feed-forward.
            let ff1 = self.feed_forward(&self.norm(&hs, &format!("{p}.norm_feed_forward1"))?, steps, &format!("{p}.feed_forward1"), &mut buf)?;
            hs.iter_mut().zip(&ff1).for_each(|(a, b)| *a += 0.5 * b);
            let attn = self.attention(&self.norm(&hs, &format!("{p}.norm_self_att"))?, steps, &pos, &format!("{p}.self_attn"), &mut buf)?;
            hs.iter_mut().zip(&attn).for_each(|(a, b)| *a += b);
            let conv = self.convolution(&self.norm(&hs, &format!("{p}.norm_conv"))?, steps, &format!("{p}.conv"), &mut buf)?;
            hs.iter_mut().zip(&conv).for_each(|(a, b)| *a += b);
            let ff2 = self.feed_forward(&self.norm(&hs, &format!("{p}.norm_feed_forward2"))?, steps, &format!("{p}.feed_forward2"), &mut buf)?;
            hs.iter_mut().zip(&ff2).for_each(|(a, b)| *a += 0.5 * b);
            hs = self.norm(&hs, &format!("{p}.norm_out"))?;
        }
        Ok((self.linear(&hs, steps, "encoder_projector", &mut buf)?, steps))
    }

    fn feed_forward(&self, x: &[f32], rows: usize, name: &str, buf: &mut Vec<f32>) -> Result<Vec<f32>> {
        let mut h = self.linear(x, rows, &format!("{name}.linear1"), buf)?;
        h.iter_mut().for_each(|v| *v = silu(*v));
        self.linear(&h, rows, &format!("{name}.linear2"), buf)
    }

    fn attention(&self, x: &[f32], steps: usize, pos: &[f32], name: &str, buf: &mut Vec<f32>) -> Result<Vec<f32>> {
        let q = self.linear(x, steps, &format!("{name}.q_proj"), buf)?;
        let k = self.linear(x, steps, &format!("{name}.k_proj"), buf)?;
        let v = self.linear(x, steps, &format!("{name}.v_proj"), buf)?;
        let span = 2 * steps - 1;
        let r = self.linear(pos, span, &format!("{name}.relative_k_proj"), buf)?;
        let bias_u = self.floats(&format!("{name}.bias_u"))?;
        let bias_v = self.floats(&format!("{name}.bias_v"))?;
        let scale = 1.0 / (HEAD as f32).sqrt();
        let mut out = vec![0f32; steps * D];
        let mut qu = vec![0f32; steps * HEAD];
        let mut qv = vec![0f32; steps * HEAD];
        let mut kh = vec![0f32; steps * HEAD];
        let mut vh = vec![0f32; steps * HEAD];
        let mut rh = vec![0f32; span * HEAD];
        for head in 0..HEADS {
            let off = head * HEAD;
            for t in 0..steps {
                for d in 0..HEAD {
                    let qq = q[t * D + off + d];
                    qu[t * HEAD + d] = qq + bias_u[off + d];
                    qv[t * HEAD + d] = qq + bias_v[off + d];
                    kh[t * HEAD + d] = k[t * D + off + d];
                    vh[t * HEAD + d] = v[t * D + off + d];
                }
            }
            for s in 0..span {
                rh[s * HEAD..(s + 1) * HEAD].copy_from_slice(&r[s * D + off..s * D + off + HEAD]);
            }
            let content = matmul_t(&qu, steps, &kh, steps, HEAD, None);
            let position = matmul_t(&qv, steps, &rh, span, HEAD, None);
            let mut weights = vec![0f32; steps * steps];
            for i in 0..steps {
                let row = &mut weights[i * steps..(i + 1) * steps];
                for (j, cell) in row.iter_mut().enumerate() {
                    // Query i, key j: relative position i − j.
                    *cell = (content[i * steps + j] + position[i * span + steps - 1 - i + j]) * scale;
                }
                let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let mut sum = 0.0;
                for cell in row.iter_mut() {
                    *cell = (*cell - max).exp();
                    sum += *cell;
                }
                row.iter_mut().for_each(|c| *c /= sum);
            }
            // weights (steps × steps) · V (steps × HEAD)
            let mut o = vec![0f32; steps * HEAD];
            // SAFETY: shapes match the strides.
            unsafe {
                matrixmultiply::sgemm(steps, steps, HEAD, 1.0, weights.as_ptr(), steps as isize, 1, vh.as_ptr(), HEAD as isize, 1, 0.0, o.as_mut_ptr(), HEAD as isize, 1);
            }
            for t in 0..steps {
                out[t * D + off..t * D + off + HEAD].copy_from_slice(&o[t * HEAD..(t + 1) * HEAD]);
            }
        }
        self.linear(&out, steps, &format!("{name}.o_proj"), buf)
    }

    fn convolution(&self, x: &[f32], steps: usize, name: &str, buf: &mut Vec<f32>) -> Result<Vec<f32>> {
        let wide = self.linear(x, steps, &format!("{name}.pointwise_conv1"), buf)?;
        // GLU: the first half gated by the second.
        let mut g = vec![0f32; steps * D];
        for t in 0..steps {
            for c in 0..D {
                g[t * D + c] = wide[t * 2 * D + c] * sigmoid(wide[t * 2 * D + D + c]);
            }
        }
        let kernel = self.floats(&format!("{name}.depthwise_conv.weight"))?;
        let ks = kernel.len() / D;
        let pad = (ks - 1) / 2;
        let (mean, var) = (self.floats(&format!("{name}.norm.running_mean"))?, self.floats(&format!("{name}.norm.running_var"))?);
        let (nw, nb) = (self.floats(&format!("{name}.norm.weight"))?, self.floats(&format!("{name}.norm.bias"))?);
        let mut y = vec![0f32; steps * D];
        for c in 0..D {
            let inv = nw[c] / (var[c] + 1e-5).sqrt();
            for t in 0..steps {
                let mut s = 0.0;
                for k in 0..ks {
                    let tt = (t + k) as isize - pad as isize;
                    if tt >= 0 && (tt as usize) < steps {
                        s += kernel[c * ks + k] * g[tt as usize * D + c];
                    }
                }
                y[t * D + c] = silu((s - mean[c]) * inv + nb[c]);
            }
        }
        self.linear(&y, steps, &format!("{name}.pointwise_conv2"), buf)
    }

    /// One step of the prediction network for `token`: its projected output.
    fn predict(&self, token: usize, state: &mut [(Vec<f32>, Vec<f32>); 2]) -> Vec<f32> {
        let mut input = self.embedding[token * H..(token + 1) * H].to_vec();
        for (layer, (w_ih, w_hh, bias)) in self.lstm.iter().enumerate() {
            let (hidden, cell) = &mut state[layer];
            let from_input = matvec(w_ih, &input, bias);
            let gates: Vec<f32> = matvec(w_hh, hidden, &from_input);
            for i in 0..H {
                let (ig, fg, gg, og) = (sigmoid(gates[i]), sigmoid(gates[H + i]), gates[2 * H + i].tanh(), sigmoid(gates[3 * H + i]));
                cell[i] = fg * cell[i] + ig * gg;
                hidden[i] = og * cell[i].tanh();
            }
            input = hidden.clone();
        }
        matmul_t(&input, 1, &self.dec_proj.0, H, H, Some(&self.dec_proj.1))
    }

    /// Speech (16 kHz mono) to text.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String> {
        let started = std::time::Instant::now();
        let (mel, frames) = features(audio);
        if frames < 9 {
            bail!("the recording is too short");
        }
        debug_time("features", started);
        let (enc, steps) = self.encode(&mel, frames)?;
        debug_time("encoder", started);
        let mut state = [(vec![0f32; H], vec![0f32; H]), (vec![0f32; H], vec![0f32; H])];
        let mut prediction = self.predict(BLANK, &mut state);
        let mut tokens = Vec::new();
        let (mut t, mut guard) = (0usize, 0usize);
        let mut joint = vec![0f32; H];
        while t < steps && guard < 10 * steps {
            guard += 1;
            for i in 0..H {
                joint[i] = (enc[t * H + i] + prediction[i]).max(0.0);
            }
            let logits = matvec(&self.head.0, &joint, &self.head.1);
            let token = argmax(&logits[..VOCAB]);
            let mut duration = DURATIONS[argmax(&logits[VOCAB..])];
            if token == BLANK {
                duration = duration.max(1);
            } else {
                tokens.push(token);
                prediction = self.predict(token, &mut state);
            }
            t += duration;
        }
        debug_time("decoding", started);
        Ok(self.detokenize(&tokens))
    }

    fn detokenize(&self, tokens: &[usize]) -> String {
        let mut text = String::new();
        for &id in tokens {
            if self.special.get(id).copied().unwrap_or(true) {
                continue;
            }
            text.push_str(&self.vocab[id]);
        }
        text.replace('▁', " ").trim().to_string()
    }
}

/// Stage timings with AIHAT_STT_DEBUG=1.
fn debug_time(stage: &str, since: std::time::Instant) {
    if std::env::var_os("AIHAT_STT_DEBUG").is_some() {
        eprintln!("stt: {stage} done at {} ms", since.elapsed().as_millis());
    }
}

fn argmax(values: &[f32]) -> usize {
    values.iter().enumerate().fold((0, f32::NEG_INFINITY), |best, (i, &v)| if v > best.1 { (i, v) } else { best }).0
}

/// Pieces by id from tokenizer.json, and which ids are special.
fn read_vocab(path: &Path) -> Result<(Vec<String>, Vec<bool>)> {
    let json: Value = serde_json::from_slice(&std::fs::read(path).with_context(|| format!("reading {}", path.display()))?).context("reading tokenizer.json")?;
    let model = json.get("model").context("tokenizer.json has no model")?;
    let mut vocab: Vec<String> = match model.get("vocab") {
        Some(Value::Array(pieces)) => pieces.iter().map(|p| p.get(0).and_then(Value::as_str).unwrap_or("").to_string()).collect(),
        Some(Value::Object(map)) => {
            let mut by_id = vec![String::new(); map.len()];
            for (piece, id) in map {
                if let Some(id) = id.as_u64().map(|i| i as usize)
                    && id < by_id.len()
                {
                    by_id[id] = piece.clone();
                }
            }
            by_id
        }
        _ => bail!("tokenizer.json has no vocabulary"),
    };
    vocab.resize(VOCAB, String::new());
    let mut special = vec![false; VOCAB];
    special[BLANK] = true;
    for added in json.get("added_tokens").and_then(Value::as_array).into_iter().flatten() {
        if added.get("special").and_then(Value::as_bool) == Some(true)
            && let Some(id) = added.get("id").and_then(Value::as_u64)
            && (id as usize) < VOCAB
        {
            special[id as usize] = true;
            vocab[id as usize] = added.get("content").and_then(Value::as_str).unwrap_or("").to_string();
        }
    }
    // Pieces like <unk> or <|endoftext|> are never spoken words.
    for (id, piece) in vocab.iter().enumerate() {
        if piece.starts_with('<') && piece.ends_with('>') {
            special[id] = true;
        }
    }
    Ok((vocab, special))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_read_like_numpy() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert!((f16_to_f32(0x0001) - 5.960_464_5e-8).abs() < 1e-12);
    }

    #[test]
    fn five_value_records_unpack() {
        // One row of 3: codes 0,1,2 → −, 0, + ; the two non-zeros: lo then hi.
        let trits = 2 * 9 + 3; // digits 0, 1, 2
        let blob = [trits as u8, 0b10, 0x00, 0x38, 0x00, 0x40]; // lo 0.5, hi 2.0
        let tensor = Tensor { shape: vec![1, 3], store: five_value(&blob, 1, 3).unwrap() };
        assert_eq!(tensor.floats(), vec![-0.5, 0.0, 2.0]);
    }

    #[test]
    fn six_bit_records_unpack() {
        // 4 values 31, -32, 0, 1 (stored +32) in 3 bytes, scale 0.5.
        let raw: [u32; 4] = [63, 0, 32, 33];
        let word = raw[0] | raw[1] << 6 | raw[2] << 12 | raw[3] << 18;
        let blob = [word as u8, (word >> 8) as u8, (word >> 16) as u8, 0x00, 0x38];
        let tensor = Tensor { shape: vec![1, 4], store: int_n(&blob, &[1, 4], 6).unwrap() };
        assert_eq!(tensor.floats(), vec![15.5, -16.0, 0.0, 0.5]);
    }

    #[test]
    fn mel_filters_are_slaney() {
        let f = mel_filters();
        // Triangles with area normalisation: never negative, and the middle ones have weight.
        assert!(f.iter().all(|w| *w >= 0.0));
        assert!(f[MELS / 2 * 257..(MELS / 2 + 1) * 257].iter().any(|w| *w > 0.0));
    }
}
