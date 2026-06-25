use serde::{Deserialize, Serialize};

/// 3-level Supaa mode.
/// `Priority` has been removed — the final design uses exactly these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Normal,
    Supaa,
    #[serde(rename = "supaa_plus")]
    SupaaPlus,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Normal    => "Normal",
            Mode::Supaa     => "Supaa",
            Mode::SupaaPlus => "Supaa++",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "normal"                               => Some(Mode::Normal),
            "supaa"                                => Some(Mode::Supaa),
            "supaa++" | "supaa_plus" | "supaaplus" => Some(Mode::SupaaPlus),
            _ => None,
        }
    }

    // ── CPU weight (cgroup v2: 1–10000, default 100) ──
    //
    // NOTE: Normal arms are structurally required by the match but are never
    // reached — policy::apply_mode() returns immediately for Normal mode before
    // calling any of these helpers.  The values (100 = kernel default) are
    // correct placeholders if the guard were ever removed.

    /// CPU weight for the focus application.
    pub fn focus_cpu_weight(&self) -> u64 {
        match self {
            Mode::Normal    => 100,   // unreachable via apply_mode (Normal is no-op)
            Mode::Supaa     => 800,
            Mode::SupaaPlus => 10000,
        }
    }

    /// CPU weight for background applications.
    pub fn bg_cpu_weight(&self) -> u64 {
        match self {
            Mode::Normal    => 100,   // unreachable via apply_mode (Normal is no-op)
            Mode::Supaa     => 20,
            Mode::SupaaPlus => 1,
        }
    }

    // ── I/O weight (cgroup v2: 1–10000, default 100) ──

    pub fn focus_io_weight(&self) -> u64 {
        match self {
            Mode::Normal    => 100,   // unreachable via apply_mode (Normal is no-op)
            Mode::Supaa     => 800,
            Mode::SupaaPlus => 10000,
        }
    }

    pub fn bg_io_weight(&self) -> u64 {
        match self {
            Mode::Normal    => 100,   // unreachable via apply_mode (Normal is no-op)
            Mode::Supaa     => 20,
            Mode::SupaaPlus => 1,
        }
    }

    // ── Memory pressure ──
    // memory.high is a soft throttle — kernel reclaims page cache more
    // aggressively but does NOT OOM-kill processes.

    /// Percentage of total RAM used as memory.high for background cgroup.
    /// None = no limit.
    pub fn bg_memory_high_percent(&self) -> Option<u64> {
        match self {
            Mode::Normal    => None,  // unreachable via apply_mode (Normal is no-op)
            Mode::Supaa     => Some(75),
            Mode::SupaaPlus => Some(50),
        }
    }

    // ── Behaviour flags ──

    /// Should non-essential background processes be frozen?
    pub fn should_freeze_background(&self) -> bool {
        matches!(self, Mode::SupaaPlus)
    }

    /// Should Hyprland workspace switching be locked?
    pub fn should_lock_workspace(&self) -> bool {
        matches!(self, Mode::SupaaPlus)
    }

    /// Does this mode require a confirmation dialog before quitting?
    pub fn requires_quit_confirmation(&self) -> bool {
        matches!(self, Mode::SupaaPlus)
    }
}

/// Per-session options the user can toggle.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActivateOptions {
    pub lock_workspace:    bool,
    pub freeze_background: bool,
}
