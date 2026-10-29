//! Who made AI Hat and where to find it, as on the phone app's About
//! screen. Shown by `/about` and `aihat about`.

pub const MAKER: &str = "Aman Mohammed";
pub const ROLE: &str = "Developer";
pub const BIO: &str = "JavaScript and TypeScript across Node, Hono, MERN, Astro and Next, and React Native with Expo. Now exploring Go, Rust, AI agents and IoT.";

pub struct Link {
    pub label: &'static str,
    /// The address as shown, or what the link is for.
    pub detail: &'static str,
    pub url: &'static str,
}

const fn link(label: &'static str, detail: &'static str, url: &'static str) -> Link {
    Link { label, detail, url }
}

/// Where to find the maker.
pub const MAKER_LINKS: [Link; 3] = [
    link("Portfolio", "amanm.space", "https://amanm.space"),
    link("GitHub", "aman-mohammed-max", "https://github.com/aman-mohammed-max"),
    link("Say hi", "connect.amanm.space", "https://connect.amanm.space"),
];

/// Where to find the app.
pub const APP_LINKS: [Link; 3] = [
    link("AI Hat CLI on GitHub", "Source code, downloads and releases", "https://github.com/aman-mohammed-max/aihat-cli"),
    link("AI Hat for phones", "The app this one shares chats and settings with", "https://github.com/aman-mohammed-max/aihat"),
    link("Report a problem", "Opens a new issue on GitHub", "https://github.com/aman-mohammed-max/aihat-cli/issues/new/choose"),
];

/// "v0.1.0 · Rust · terminal"
pub fn version_line() -> String {
    format!("v{} · Rust · terminal", env!("CARGO_PKG_VERSION"))
}

/// Every link, in the order the About screen lists them.
pub fn links() -> impl Iterator<Item = &'static Link> {
    MAKER_LINKS.iter().chain(APP_LINKS.iter())
}
