use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;

pub const FASTPLAY_PRO_PRODUCT_ID: u64 = 1_195_401;
pub const FASTPLAY_PRO_BUY_URL: &str =
    "https://fastcast.lemonsqueezy.com/checkout/buy/f71de58c-73cc-4688-be25-b543f9f3401c";
const LICENSE_API_BASE: &str = "https://api.lemonsqueezy.com/v1/licenses";
const OFFLINE_GRACE_MS: u64 = 14 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseTier {
    Free,
    Pro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseSource {
    Free,
    DevelopmentOverride,
    StoredLicense,
    OfflineGrace,
    NeedsValidation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseState {
    pub tier: LicenseTier,
    pub status_label: String,
    pub source: LicenseSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredLicense {
    pub license_key: String,
    pub instance_id: String,
    pub product_id: u64,
    pub license_status: String,
    pub customer_email: Option<String>,
    pub last_validated_unix_ms: u64,
}

#[derive(Debug)]
pub struct LicenseActivationResult {
    pub state: LicenseState,
    pub customer_email: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseErrorKind {
    Local,
    Transient,
    Authoritative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseError {
    kind: LicenseErrorKind,
    message: String,
}

impl LicenseError {
    fn local(message: impl Into<String>) -> Self {
        Self {
            kind: LicenseErrorKind::Local,
            message: message.into(),
        }
    }

    fn transient(message: impl Into<String>) -> Self {
        Self {
            kind: LicenseErrorKind::Transient,
            message: message.into(),
        }
    }

    fn authoritative(message: impl Into<String>) -> Self {
        Self {
            kind: LicenseErrorKind::Authoritative,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> LicenseErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for LicenseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LicenseError {}

impl Default for LicenseState {
    fn default() -> Self {
        Self {
            tier: LicenseTier::Free,
            status_label: "FastPlay Free".to_string(),
            source: LicenseSource::Free,
        }
    }
}

impl LicenseState {
    pub fn detect() -> Self {
        Self::detect_with(
            std::env::var("FASTPLAY_PRO_DEV").ok().as_deref(),
            StoredLicense::load().ok().flatten(),
            now_unix_ms(),
        )
    }

    fn detect_with(dev_override: Option<&str>, stored: Option<StoredLicense>, now_ms: u64) -> Self {
        if let Some(state) = Self::from_dev_override_value(dev_override) {
            return state;
        }
        if let Some(stored) = stored {
            return Self::from_stored_license(&stored, now_ms);
        }
        Self::default()
    }

    fn from_dev_override_value(value: Option<&str>) -> Option<Self> {
        match value.map(str::trim) {
            Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") => Some(Self {
                tier: LicenseTier::Pro,
                status_label: "FastPlay Pro (development override)".to_string(),
                source: LicenseSource::DevelopmentOverride,
            }),
            _ => None,
        }
    }

    fn from_stored_license(stored: &StoredLicense, now_ms: u64) -> Self {
        if stored.product_id != FASTPLAY_PRO_PRODUCT_ID {
            return Self {
                tier: LicenseTier::Free,
                status_label: "FastPlay Free (license product mismatch)".to_string(),
                source: LicenseSource::NeedsValidation,
            };
        }
        if !license_status_allows_pro(&stored.license_status) {
            let status = license_status_label(&stored.license_status);
            return Self {
                tier: LicenseTier::Free,
                status_label: format!("FastPlay Free (license {status})"),
                source: LicenseSource::NeedsValidation,
            };
        }
        if stored.last_validated_unix_ms == 0 {
            return Self {
                tier: LicenseTier::Pro,
                status_label: "FastPlay Pro".to_string(),
                source: LicenseSource::StoredLicense,
            };
        }
        let age_ms = now_ms.saturating_sub(stored.last_validated_unix_ms);
        if age_ms <= OFFLINE_GRACE_MS {
            Self {
                tier: LicenseTier::Pro,
                status_label: "FastPlay Pro".to_string(),
                source: LicenseSource::StoredLicense,
            }
        } else {
            Self {
                tier: LicenseTier::Pro,
                status_label: "FastPlay Pro (offline grace)".to_string(),
                source: LicenseSource::OfflineGrace,
            }
        }
    }

    pub fn should_validate_on_startup(&self) -> bool {
        matches!(
            self.source,
            LicenseSource::StoredLicense | LicenseSource::OfflineGrace
        )
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

impl StoredLicense {
    pub fn load() -> io::Result<Option<Self>> {
        let Some(path) = storage_path() else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let contents = fs::read_to_string(path)?;
        Ok(Self::parse(&contents))
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = storage_path() else {
            return Ok(());
        };
        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.serialize())
    }

    fn parse(contents: &str) -> Option<Self> {
        let mut license_key = None;
        let mut instance_id = None;
        let mut product_id = None;
        let mut license_status = None;
        let mut customer_email = None;
        let mut last_validated_unix_ms = None;

        for line in contents.lines() {
            let Some((key, value)) = line.split_once('\t') else {
                continue;
            };
            let value = unescape_field(value)?;
            match key {
                "license_key" => license_key = Some(value),
                "instance_id" => instance_id = Some(value),
                "product_id" => product_id = value.parse::<u64>().ok(),
                "license_status" => license_status = Some(value),
                "customer_email" if !value.is_empty() => customer_email = Some(value),
                "last_validated_unix_ms" => last_validated_unix_ms = value.parse::<u64>().ok(),
                _ => {}
            }
        }

        Some(Self {
            license_key: license_key.filter(|value| !value.trim().is_empty())?,
            instance_id: instance_id.filter(|value| !value.trim().is_empty())?,
            product_id: product_id?,
            license_status: license_status.unwrap_or_else(|| "active".to_string()),
            customer_email,
            last_validated_unix_ms: last_validated_unix_ms.unwrap_or(0),
        })
    }

    fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(&field_line("license_key", &self.license_key));
        out.push_str(&field_line("instance_id", &self.instance_id));
        out.push_str(&field_line("product_id", &self.product_id.to_string()));
        out.push_str(&field_line("license_status", &self.license_status));
        out.push_str(&field_line(
            "customer_email",
            self.customer_email.as_deref().unwrap_or(""),
        ));
        out.push_str(&field_line(
            "last_validated_unix_ms",
            &self.last_validated_unix_ms.to_string(),
        ));
        out
    }
}

#[allow(dead_code)]
pub fn activate_license_key(license_key: &str) -> Result<LicenseActivationResult, LicenseError> {
    activate_license_key_if_current(license_key, || true)
}

pub fn activate_license_key_if_current(
    license_key: &str,
    is_current: impl Fn() -> bool,
) -> Result<LicenseActivationResult, LicenseError> {
    let license_key = license_key.trim();
    if license_key.is_empty() {
        return Err(LicenseError::local("License key required"));
    }

    let instance_name = instance_name();
    let response = post_license_form(
        "activate",
        &[
            ("license_key", license_key),
            ("instance_name", &instance_name),
        ],
    )?;
    if response.activated != Some(true) {
        return Err(LicenseError::authoritative(
            response
                .error
                .unwrap_or_else(|| "License activation failed".to_string()),
        ));
    }
    let instance_id = response
        .instance
        .as_ref()
        .map(|instance| instance.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            LicenseError::transient("Activation response did not include an instance id")
        })?;
    let product_id = response
        .meta
        .as_ref()
        .and_then(|meta| meta.product_id_u64());
    if product_id != Some(FASTPLAY_PRO_PRODUCT_ID) {
        best_effort_deactivate_instance(license_key, &instance_id);
        return Err(LicenseError::authoritative(
            "That license is not for FastPlay Pro",
        ));
    }
    let license_status = response
        .license_key
        .as_ref()
        .map(|key| key.status.clone())
        .unwrap_or_else(|| "active".to_string());
    if !license_status_allows_pro(&license_status) {
        best_effort_deactivate_instance(license_key, &instance_id);
        return Err(LicenseError::authoritative(format!(
            "License is {}",
            license_status_label(&license_status)
        )));
    }

    let customer_email = response.meta.as_ref().and_then(|meta| {
        meta.customer_email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    });
    let stored = StoredLicense {
        license_key: license_key.to_string(),
        instance_id,
        product_id: FASTPLAY_PRO_PRODUCT_ID,
        license_status,
        customer_email: customer_email.clone(),
        last_validated_unix_ms: now_unix_ms(),
    };
    if !is_current() {
        best_effort_deactivate_instance(license_key, &stored.instance_id);
        return Err(LicenseError::transient("License operation was superseded"));
    }
    stored.save().map_err(|error| {
        LicenseError::local(format!("Activated, but could not save license: {error}"))
    })?;
    Ok(LicenseActivationResult {
        state: LicenseState::from_stored_license(&stored, now_unix_ms()),
        customer_email,
    })
}

#[allow(dead_code)]
pub fn validate_stored_license() -> Result<LicenseState, LicenseError> {
    validate_stored_license_if_current(|| true)
}

pub fn validate_stored_license_if_current(
    is_current: impl Fn() -> bool,
) -> Result<LicenseState, LicenseError> {
    let stored = StoredLicense::load()
        .map_err(|error| LicenseError::local(format!("Could not read saved license: {error}")))?
        .ok_or_else(|| LicenseError::local("No saved license"))?;
    let response = match post_license_form(
        "validate",
        &[
            ("license_key", &stored.license_key),
            ("instance_id", &stored.instance_id),
        ],
    ) {
        Ok(response) => response,
        Err(error) if error.kind() == LicenseErrorKind::Authoritative => {
            let status = status_from_authoritative_error(error.message());
            return persist_stored_denial(stored, &status, None, &is_current);
        }
        Err(error) => return Err(error),
    };
    if response.valid == Some(false) {
        return persist_validation_denial(stored, &response, "invalid", &is_current);
    }
    if response.valid != Some(true) {
        return Err(LicenseError::transient(response.error.unwrap_or_else(
            || "License validation response was incomplete".to_string(),
        )));
    }
    let product_id = response
        .meta
        .as_ref()
        .and_then(|meta| meta.product_id_u64());
    if let Some(product_id) = product_id {
        if product_id != FASTPLAY_PRO_PRODUCT_ID {
            return persist_validation_denial(stored, &response, "wrong_product", &is_current);
        }
    } else {
        return Err(LicenseError::transient(
            "License validation response did not include a product id",
        ));
    }
    let license_status = response
        .license_key
        .as_ref()
        .map(|key| key.status.clone())
        .unwrap_or_else(|| stored.license_status.clone());
    if !license_status_allows_pro(&license_status) {
        return persist_validation_denial(stored, &response, &license_status, &is_current);
    }
    let customer_email = response
        .meta
        .as_ref()
        .and_then(|meta| meta.customer_email.clone())
        .or(stored.customer_email);
    let updated = StoredLicense {
        license_status,
        customer_email,
        last_validated_unix_ms: now_unix_ms(),
        ..stored
    };
    if !is_current() {
        return Err(LicenseError::transient("License operation was superseded"));
    }
    updated.save().map_err(|error| {
        LicenseError::local(format!("Validated, but could not save license: {error}"))
    })?;
    Ok(LicenseState::from_stored_license(&updated, now_unix_ms()))
}

#[allow(dead_code)]
pub fn deactivate_stored_license() -> Result<LicenseState, LicenseError> {
    deactivate_stored_license_if_current(|| true)
}

pub fn deactivate_stored_license_if_current(
    is_current: impl Fn() -> bool,
) -> Result<LicenseState, LicenseError> {
    let stored = StoredLicense::load()
        .map_err(|error| LicenseError::local(format!("Could not read saved license: {error}")))?
        .ok_or_else(|| LicenseError::local("No saved license to deactivate"))?;
    let response = match post_license_form(
        "deactivate",
        &[
            ("license_key", &stored.license_key),
            ("instance_id", &stored.instance_id),
        ],
    ) {
        Ok(response) => response,
        Err(error)
            if error.kind() == LicenseErrorKind::Authoritative
                && deactivation_error_allows_local_clear(error.message()) =>
        {
            if !is_current() {
                return Err(LicenseError::transient("License operation was superseded"));
            }
            clear_stored_license().map_err(|clear_error| {
                LicenseError::local(format!(
                    "Activation is already gone, but could not clear local license: {clear_error}"
                ))
            })?;
            return Ok(LicenseState::detect());
        }
        Err(error) => return Err(error),
    };
    if response.deactivated != Some(true) {
        let message = response
            .error
            .unwrap_or_else(|| "License deactivation failed".to_string());
        if deactivation_error_allows_local_clear(&message) {
            if !is_current() {
                return Err(LicenseError::transient("License operation was superseded"));
            }
            clear_stored_license().map_err(|clear_error| {
                LicenseError::local(format!(
                    "Activation is already gone, but could not clear local license: {clear_error}"
                ))
            })?;
            return Ok(LicenseState::detect());
        }
        return Err(LicenseError::authoritative(message));
    }
    if !is_current() {
        return Err(LicenseError::transient("License operation was superseded"));
    }
    clear_stored_license().map_err(|error| {
        LicenseError::local(format!("Deactivated, but could not clear license: {error}"))
    })?;
    Ok(LicenseState::detect())
}

pub fn clear_stored_license() -> io::Result<()> {
    let Some(path) = storage_path() else {
        return Ok(());
    };
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn persist_validation_denial(
    stored: StoredLicense,
    response: &LicenseApiResponse,
    fallback_status: &str,
    is_current: &impl Fn() -> bool,
) -> Result<LicenseState, LicenseError> {
    let status = response
        .license_key
        .as_ref()
        .map(|key| key.status.as_str())
        .unwrap_or(fallback_status);
    let customer_email = response
        .meta
        .as_ref()
        .and_then(|meta| meta.customer_email.clone());
    persist_stored_denial(stored, status, customer_email, is_current)
}

fn persist_stored_denial(
    stored: StoredLicense,
    status: &str,
    customer_email: Option<String>,
    is_current: &impl Fn() -> bool,
) -> Result<LicenseState, LicenseError> {
    let updated = denied_stored_license(stored, status, customer_email, now_unix_ms());
    if !is_current() {
        return Err(LicenseError::transient("License operation was superseded"));
    }
    updated.save().map_err(|error| {
        LicenseError::local(format!(
            "License is {}, but could not update local license: {error}",
            license_status_label(status)
        ))
    })?;
    Ok(LicenseState::from_stored_license(&updated, now_unix_ms()))
}

fn denied_stored_license(
    stored: StoredLicense,
    status: &str,
    customer_email: Option<String>,
    now_ms: u64,
) -> StoredLicense {
    let customer_email = customer_email.or(stored.customer_email);
    StoredLicense {
        license_status: normalized_denied_status(status),
        customer_email,
        last_validated_unix_ms: now_ms,
        ..stored
    }
}

fn status_from_authoritative_error(message: &str) -> String {
    let message = message.to_ascii_lowercase();
    for status in ["disabled", "expired", "refunded", "revoked", "invalid"] {
        if message.contains(status) {
            return status.to_string();
        }
    }
    "invalid".to_string()
}

fn normalized_denied_status(status: &str) -> String {
    let status = status.trim();
    if status.is_empty() {
        "invalid".to_string()
    } else {
        let normalized = status
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ' '))
            .take(40)
            .collect::<String>()
            .trim()
            .to_string();
        if normalized.is_empty() {
            "invalid".to_string()
        } else {
            normalized
        }
    }
}

fn best_effort_deactivate_instance(license_key: &str, instance_id: &str) {
    let _ = post_license_form(
        "deactivate",
        &[("license_key", license_key), ("instance_id", instance_id)],
    );
}

fn post_license_form(
    endpoint: &str,
    fields: &[(&str, &str)],
) -> Result<LicenseApiResponse, LicenseError> {
    let url = format!("{LICENSE_API_BASE}/{endpoint}");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(12))
        .timeout_write(Duration::from_secs(8))
        .build();
    let response = agent
        .post(&url)
        .set("Accept", "application/json")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_form(fields)
        .map_err(format_ureq_error)?;
    response.into_json::<LicenseApiResponse>().map_err(|error| {
        LicenseError::transient(format!("Could not parse license response: {error}"))
    })
}

fn format_ureq_error(error: ureq::Error) -> LicenseError {
    match error {
        ureq::Error::Status(status, response) => {
            let message = response
                .into_json::<LicenseApiResponse>()
                .ok()
                .and_then(|body| body.error)
                .unwrap_or_else(|| "License server returned an error".to_string());
            if status >= 500 {
                LicenseError::transient(message)
            } else {
                LicenseError::authoritative(message)
            }
        }
        ureq::Error::Transport(error) => {
            LicenseError::transient(format!("Could not reach license server: {error}"))
        }
    }
}

fn license_status_allows_pro(status: &str) -> bool {
    matches!(status, "active" | "inactive")
}

fn license_status_label(status: &str) -> String {
    let status = status.trim();
    if status.is_empty() {
        return "invalid".to_string();
    }
    status
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ' '))
        .take(40)
        .collect::<String>()
        .trim()
        .to_string()
}

fn deactivation_error_allows_local_clear(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "already deactivated",
        "already been deactivated",
        "activation not found",
        "instance not found",
        "activation does not exist",
        "instance does not exist",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn instance_name() -> String {
    let computer = std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Windows".to_string());
    format!("FastPlay on {computer}")
}

fn storage_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("FastPlay").join("license.tsv"))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn field_line(key: &str, value: &str) -> String {
    format!("{key}\t{}\n", escape_field(value))
}

fn escape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_field(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            't' => out.push('\t'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            _ => return None,
        }
    }
    Some(out)
}

#[derive(Debug, Deserialize)]
struct LicenseApiResponse {
    activated: Option<bool>,
    valid: Option<bool>,
    deactivated: Option<bool>,
    error: Option<String>,
    license_key: Option<ApiLicenseKey>,
    instance: Option<ApiInstance>,
    meta: Option<ApiMeta>,
}

#[derive(Debug, Deserialize)]
struct ApiLicenseKey {
    status: String,
}

#[derive(Debug, Deserialize)]
struct ApiInstance {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ApiMeta {
    product_id: Option<serde_json::Value>,
    customer_email: Option<String>,
}

impl ApiMeta {
    fn product_id_u64(&self) -> Option<u64> {
        match self.product_id.as_ref()? {
            serde_json::Value::Number(number) => number.as_u64(),
            serde_json::Value::String(value) => value.parse::<u64>().ok(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stored(last_validated_unix_ms: u64) -> StoredLicense {
        StoredLicense {
            license_key: "key\\with\ttab".to_string(),
            instance_id: "instance".to_string(),
            product_id: FASTPLAY_PRO_PRODUCT_ID,
            license_status: "active".to_string(),
            customer_email: Some("test@example.com".to_string()),
            last_validated_unix_ms,
        }
    }

    fn validation_response(
        status: Option<&str>,
        customer_email: Option<&str>,
    ) -> LicenseApiResponse {
        LicenseApiResponse {
            activated: None,
            valid: Some(false),
            deactivated: None,
            error: Some("License is no longer valid".to_string()),
            license_key: status.map(|status| ApiLicenseKey {
                status: status.to_string(),
            }),
            instance: None,
            meta: Some(ApiMeta {
                product_id: Some(serde_json::Value::Number(FASTPLAY_PRO_PRODUCT_ID.into())),
                customer_email: customer_email.map(str::to_string),
            }),
        }
    }

    #[test]
    fn defaults_to_free() {
        let license = LicenseState::detect_with(None, None, 1_000);
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
        let license = LicenseState::detect_with(Some("1"), None, 1_000);
        assert_eq!(license.tier, LicenseTier::Pro);
        assert_eq!(license.source, LicenseSource::DevelopmentOverride);
        assert!(license.status_label.contains("development override"));
        assert!(license.can_save_markers());
        assert!(license.can_edit_marker_notes());
        assert!(license.can_export_markers());
        assert!(license.can_batch_export_marker_screenshots());
        assert!(license.can_save_review_queues());
        assert!(license.can_load_review_queues());
        assert!(license.can_delete_review_queues());
    }

    #[test]
    fn dev_override_wins_over_stored_license() {
        let mut stored = sample_stored(1_000);
        stored.product_id = 99;
        let license = LicenseState::detect_with(Some("yes"), Some(stored), 1_000);
        assert_eq!(license.tier, LicenseTier::Pro);
        assert_eq!(license.source, LicenseSource::DevelopmentOverride);
    }

    #[test]
    fn stored_license_enables_pro_within_grace() {
        let stored = sample_stored(1_000);
        let license = LicenseState::detect_with(None, Some(stored), 1_000 + OFFLINE_GRACE_MS);
        assert_eq!(license.tier, LicenseTier::Pro);
        assert_eq!(license.source, LicenseSource::StoredLicense);
    }

    #[test]
    fn stale_stored_license_uses_offline_grace_label() {
        let stored = sample_stored(1_000);
        let license = LicenseState::detect_with(None, Some(stored), 1_001 + OFFLINE_GRACE_MS);
        assert_eq!(license.tier, LicenseTier::Pro);
        assert_eq!(license.source, LicenseSource::OfflineGrace);
        assert!(license.status_label.contains("offline grace"));
    }

    #[test]
    fn wrong_product_does_not_enable_pro() {
        let mut stored = sample_stored(1_000);
        stored.product_id = 99;
        let license = LicenseState::detect_with(None, Some(stored), 1_000);
        assert_eq!(license.tier, LicenseTier::Free);
        assert_eq!(license.source, LicenseSource::NeedsValidation);
    }

    #[test]
    fn disabled_license_does_not_enable_pro() {
        let mut stored = sample_stored(1_000);
        stored.license_status = "disabled".to_string();
        let license = LicenseState::detect_with(None, Some(stored), 1_000);
        assert_eq!(license.tier, LicenseTier::Free);
        assert_eq!(license.source, LicenseSource::NeedsValidation);
    }

    #[test]
    fn stored_license_roundtrips_escaped_fields() {
        let stored = sample_stored(42);
        let serialized = stored.serialize();
        let parsed = StoredLicense::parse(&serialized).unwrap();
        assert_eq!(parsed, stored);
    }

    #[test]
    fn denied_validation_response_downgrades_stored_license_to_free() {
        let stored = sample_stored(1_000);
        let response = validation_response(Some("disabled"), Some("new@example.com"));
        let updated = denied_stored_license(
            stored,
            "disabled",
            response.meta.and_then(|meta| meta.customer_email),
            2_000,
        );

        assert_eq!(updated.license_key, "key\\with\ttab");
        assert_eq!(updated.instance_id, "instance");
        assert_eq!(updated.product_id, FASTPLAY_PRO_PRODUCT_ID);
        assert_eq!(updated.license_status, "disabled");
        assert_eq!(updated.customer_email.as_deref(), Some("new@example.com"));
        assert_eq!(updated.last_validated_unix_ms, 2_000);

        let license = LicenseState::detect_with(None, Some(updated), 2_000);
        assert_eq!(license.tier, LicenseTier::Free);
        assert_eq!(license.source, LicenseSource::NeedsValidation);
        assert_eq!(license.status_label, "FastPlay Free (license disabled)");
    }

    #[test]
    fn denied_validation_without_status_stores_invalid_status() {
        let stored = sample_stored(1_000);
        let response = validation_response(None, None);
        let updated = denied_stored_license(
            stored,
            "invalid",
            response.meta.and_then(|meta| meta.customer_email),
            2_000,
        );

        assert_eq!(updated.license_status, "invalid");
        assert_eq!(updated.customer_email.as_deref(), Some("test@example.com"));
        let license = LicenseState::detect_with(None, Some(updated), 2_000);
        assert_eq!(license.tier, LicenseTier::Free);
        assert_eq!(license.status_label, "FastPlay Free (license invalid)");
    }

    #[test]
    fn deactivation_local_clear_is_allowed_only_for_already_gone_messages() {
        assert!(deactivation_error_allows_local_clear(
            "Activation not found for this instance"
        ));
        assert!(deactivation_error_allows_local_clear(
            "This instance has already been deactivated"
        ));
        assert!(!deactivation_error_allows_local_clear(
            "License server returned an error"
        ));
        assert!(!deactivation_error_allows_local_clear(
            "License key is invalid"
        ));
    }

    #[test]
    fn api_meta_accepts_numeric_or_string_product_id() {
        let numeric: ApiMeta = serde_json::from_str(r#"{"product_id":1195401}"#).unwrap();
        let string: ApiMeta = serde_json::from_str(r#"{"product_id":"1195401"}"#).unwrap();
        assert_eq!(numeric.product_id_u64(), Some(FASTPLAY_PRO_PRODUCT_ID));
        assert_eq!(string.product_id_u64(), Some(FASTPLAY_PRO_PRODUCT_ID));
    }
}
