#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
	Discord,
	Fluxer,
}

impl PlatformKind {
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Discord => "discord",
			Self::Fluxer => "fluxer",
		}
	}

	pub const fn default_prefix_enabled(self) -> bool {
		match self {
			Self::Discord => false,
			Self::Fluxer => true,
		}
	}
}
