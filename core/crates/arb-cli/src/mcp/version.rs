//! Negotiated MCP capabilities. Versions are ordered by publication date.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ProtocolVersion {
    V2024_11_05,
    V2025_03_26,
    V2025_06_18,
    #[default]
    V2025_11_25,
}

impl ProtocolVersion {
    /// Every supported revision, in [`Self::index`] order.
    pub(super) const ALL: [Self; 4] = [
        Self::V2024_11_05,
        Self::V2025_03_26,
        Self::V2025_06_18,
        Self::V2025_11_25,
    ];

    pub(super) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|version| version.as_str() == value)
    }

    /// Position in [`Self::ALL`], for per-version caches.
    pub(super) const fn index(self) -> usize {
        match self {
            Self::V2024_11_05 => 0,
            Self::V2025_03_26 => 1,
            Self::V2025_06_18 => 2,
            Self::V2025_11_25 => 3,
        }
    }

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::V2024_11_05 => "2024-11-05",
            Self::V2025_03_26 => "2025-03-26",
            Self::V2025_06_18 => "2025-06-18",
            Self::V2025_11_25 => "2025-11-25",
        }
    }

    pub(super) fn supports_structured_content(self) -> bool {
        self >= Self::V2025_06_18
    }
    pub(super) fn supports_resource_link_block(self) -> bool {
        self >= Self::V2025_06_18
    }
    pub(super) fn supports_tasks(self) -> bool {
        self >= Self::V2025_11_25
    }
    pub(super) fn supports_batch(self) -> bool {
        self <= Self::V2025_03_26
    }
    pub(super) fn allows_legacy_screenshot_overwrite(self) -> bool {
        self <= Self::V2025_06_18
    }
    pub(super) fn legacy_result_fields(self) -> bool {
        self <= Self::V2025_06_18
    }
    pub(super) fn allows_legacy_trash_original(self) -> bool {
        self <= Self::V2025_06_18
    }
    pub(super) fn accepts_assert_abbreviation(self) -> bool {
        self <= Self::V2025_03_26
    }
}
