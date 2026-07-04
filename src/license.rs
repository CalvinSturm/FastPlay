#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseTier {
    Free,
    Pro,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseState {
    pub tier: LicenseTier,
    pub status_label: String,
}

impl Default for LicenseState {
    fn default() -> Self {
        Self {
            tier: LicenseTier::Free,
            status_label: "FastPlay Free".to_string(),
        }
    }
}

impl LicenseState {
    pub fn detect() -> Self {
        Self::from_dev_override_value(std::env::var("FASTPLAY_PRO_DEV").ok().as_deref())
    }

    fn from_dev_override_value(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") => Self {
                tier: LicenseTier::Pro,
                status_label: "FastPlay Pro (development override)".to_string(),
            },
            _ => Self::default(),
        }
    }

    pub fn can_save_markers(&self) -> bool {
        self.tier == LicenseTier::Pro
    }

    pub fn can_edit_marker_notes(&self) -> bool {
        self.tier == LicenseTier::Pro
    }

    pub fn can_export_markers(&self) -> bool {
        self.tier == LicenseTier::Pro
    }

    pub fn can_batch_export_marker_screenshots(&self) -> bool {
        self.tier == LicenseTier::Pro
    }

    #[allow(dead_code)]
    pub fn can_save_review_queues(&self) -> bool {
        self.tier == LicenseTier::Pro
    }

    pub fn can_load_review_queues(&self) -> bool {
        self.tier == LicenseTier::Pro
    }

    pub fn can_delete_review_queues(&self) -> bool {
        self.tier == LicenseTier::Pro
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_free() {
        let license = LicenseState::from_dev_override_value(None);
        assert_eq!(license.tier, LicenseTier::Free);
        assert!(!license.can_save_markers());
        assert!(!license.can_edit_marker_notes());
        assert!(!license.can_export_markers());
        assert!(!license.can_batch_export_marker_screenshots());
        assert!(!license.can_save_review_queues());
        assert!(!license.can_load_review_queues());
        assert!(!license.can_delete_review_queues());
    }

    #[test]
    fn dev_override_enables_pro_capabilities() {
        let license = LicenseState::from_dev_override_value(Some("1"));
        assert_eq!(license.tier, LicenseTier::Pro);
        assert!(license.status_label.contains("development override"));
        assert!(license.can_save_markers());
        assert!(license.can_edit_marker_notes());
        assert!(license.can_export_markers());
        assert!(license.can_batch_export_marker_screenshots());
        assert!(license.can_save_review_queues());
        assert!(license.can_load_review_queues());
        assert!(license.can_delete_review_queues());
    }
}
