// SPDX-License-Identifier: MIT
// SigmaOS-fprintd - Fingerprint Authentication Daemon component
// fprintd-style fingerprint authentication service for SigmaOS,
// inspired by Arch's fprintd package (libfprint-based).

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

// ============================================================================
// FINGERPRINT DEVICE
// ============================================================================

/// Fingerprint sensor type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorType {
    /// Optical sensor
    Optical,
    /// Capacitive sensor
    Capacitive,
    /// Ultrasonic sensor
    Ultrasonic,
}

/// Fingerprint device state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// Device is present and ready
    Present,
    /// Device is being initialized
    Initializing,
    /// Device has an error
    Error,
    /// Device is removed
    Removed,
}

/// A fingerprint reader device
#[derive(Debug, Clone)]
pub struct FingerprintDevice {
    /// Device ID (e.g. "usb:8087:0a2b")
    pub id: String,
    /// Device name
    pub name: String,
    /// Sensor type
    pub sensor_type: SensorType,
    /// Current state
    pub state: DeviceState,
    /// Whether device supports identification (1:N matching)
    pub supports_identify: bool,
    /// Whether device supports verification (1:1 matching)
    pub supports_verify: bool,
    /// Number of fingerprints stored
    pub stored_fingerprints: u32,
    /// Maximum fingerprints supported
    pub max_fingerprints: u32,
    /// Whether device is claimable by a user
    pub claimed_by: Option<String>, // username
}

impl FingerprintDevice {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            sensor_type: SensorType::Capacitive,
            state: DeviceState::Present,
            supports_identify: true,
            supports_verify: true,
            stored_fingerprints: 0,
            max_fingerprints: 10,
            claimed_by: None,
        }
    }

    /// Whether device has capacity for more fingerprints
    pub fn has_capacity(&self) -> bool {
        self.stored_fingerprints < self.max_fingerprints
    }

    /// Whether device is available for enrollment
    pub fn is_enrollable(&self) -> bool {
        self.state == DeviceState::Present && self.has_capacity()
    }
}

// ============================================================================
// FINGERPRINT TEMPLATE
// ============================================================================

/// A stored fingerprint template
#[derive(Debug, Clone)]
pub struct FingerprintTemplate {
    /// Unique template ID
    pub id: String,
    /// Username this fingerprint belongs to
    pub username: String,
    /// Finger name (e.g. "right-index-finger")
    pub finger: String,
    /// Template data (simulated — real impl stores binary template)
    pub data: Vec<u8>,
    /// When enrolled
    pub enrolled_ms: u64,
    /// Device ID that enrolled this
    pub device_id: String,
}

impl FingerprintTemplate {
    /// Finger position names
    pub const FINGERS: &[&str] = &[
        "left-pinky-finger", "left-ring-finger", "left-middle-finger",
        "left-index-finger", "left-thumb", "right-thumb", "right-index-finger",
        "right-middle-finger", "right-ring-finger", "right-pinky-finger",
    ];

    /// Validate finger name
    pub fn is_valid_finger(finger: &str) -> bool {
        Self::FINGERS.contains(&finger)
    }
}

// ============================================================================
// AUTHENTICATION
// ============================================================================

/// Authentication result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthResult {
    /// Authentication succeeded
    Success,
    /// Authentication failed (wrong finger)
    Failure,
    /// Authentication was cancelled
    Cancelled,
    /// Timeout waiting for finger
    Timeout,
    /// Device error
    Error,
}

impl AuthResult {
    pub fn is_success(&self) -> bool { *self == Self::Success }
}

/// An authentication session
#[derive(Debug, Clone)]
pub struct AuthSession {
    /// Session ID
    pub id: String,
    /// Username to authenticate
    pub username: String,
    /// Device being used
    pub device_id: String,
    /// Whether this is identify (1:N) or verify (1:1)
    pub is_identify: bool,
    /// Result (None = in progress)
    pub result: Option<AuthResult>,
    /// When session started
    pub started_ms: u64,
}

// ============================================================================
// FPRINTD SERVICE
// ============================================================================

/// The fprintd daemon
pub struct FingerprintService {
    devices: Mutex<HashMap<String, FingerprintDevice>>,
    /// Templates per user: username -> Vec<template>
    templates: Mutex<HashMap<String, Vec<FingerprintTemplate>>>,
    /// Active sessions
    sessions: Mutex<HashMap<String, AuthSession>>,
    /// Whether service is running
    running: Mutex<bool>,
}

impl FingerprintService {
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
            templates: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            running: Mutex::new(false),
        }
    }

    pub fn start(&self) { *self.running.lock().unwrap() = true; }
    pub fn stop(&self) { *self.running.lock().unwrap() = false; }
    pub fn is_running(&self) -> bool { *self.running.lock().unwrap() }

    // ---- Device management ----

    pub fn add_device(&self, device: FingerprintDevice) {
        self.devices.lock().unwrap().insert(device.id.clone(), device);
    }

    pub fn remove_device(&self, id: &str) {
        self.devices.lock().unwrap().remove(id);
    }

    pub fn get_device(&self, id: &str) -> Option<FingerprintDevice> {
        self.devices.lock().unwrap().get(id).cloned()
    }

    pub fn list_devices(&self) -> Vec<FingerprintDevice> {
        self.devices.lock().unwrap().values().cloned().collect()
    }

    /// Claim a device for a user (exclusive access)
    pub fn claim_device(&self, device_id: &str, username: &str) -> Result<(), FprintdError> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices.get_mut(device_id)
            .ok_or_else(|| FprintdError::DeviceNotFound(device_id.to_string()))?;
        if device.claimed_by.is_some() && device.claimed_by.as_deref() != Some(username) {
            return Err(FprintdError::DeviceClaimed(device.claimed_by.clone().unwrap()));
        }
        device.claimed_by = Some(username.to_string());
        Ok(())
    }

    pub fn release_device(&self, device_id: &str, username: &str) -> Result<(), FprintdError> {
        let mut devices = self.devices.lock().unwrap();
        let device = devices.get_mut(device_id)
            .ok_or_else(|| FprintdError::DeviceNotFound(device_id.to_string()))?;
        if device.claimed_by.as_deref() != Some(username) {
            return Err(FprintdError::DeviceNotClaimed);
        }
        device.claimed_by = None;
        Ok(())
    }

    // ---- Template management ----

    /// Enroll a fingerprint for a user
    pub fn enroll(&self, username: &str, device_id: &str, finger: &str) -> Result<FingerprintTemplate, FprintdError> {
        if !self.is_running() {
            return Err(FprintdError::NotRunning);
        }
        if !FingerprintTemplate::is_valid_finger(finger) {
            return Err(FprintdError::InvalidFinger(finger.to_string()));
        }

        let devices = self.devices.lock().unwrap();
        let device = devices.get(device_id)
            .ok_or_else(|| FprintdError::DeviceNotFound(device_id.to_string()))?;
        if !device.is_enrollable() {
            return Err(FprintdError::DeviceNotEnrollable);
        }
        drop(devices);

        let template = FingerprintTemplate {
            id: format!("{}-{}-{}", username, finger, now_ms()),
            username: username.to_string(),
            finger: finger.to_string(),
            data: vec![0u8; 512], // simulated template data
            enrolled_ms: now_ms(),
            device_id: device_id.to_string(),
        };

        let mut templates = self.templates.lock().unwrap();
        templates.entry(username.to_string()).or_default().push(template.clone());

        // Update device count
        if let Some(d) = self.devices.lock().unwrap().get_mut(device_id) {
            d.stored_fingerprints += 1;
        }

        Ok(template)
    }

    /// Delete a fingerprint template
    pub fn delete_template(&self, template_id: &str) -> Result<(), FprintdError> {
        let mut templates = self.templates.lock().unwrap();
        for user_templates in templates.values_mut() {
            if let Some(pos) = user_templates.iter().position(|t| t.id == template_id) {
                let template = user_templates.remove(pos);
                // Update device count
                if let Some(d) = self.devices.lock().unwrap().get_mut(&template.device_id) {
                    d.stored_fingerprints = d.stored_fingerprints.saturating_sub(1);
                }
                return Ok(());
            }
        }
        Err(FprintdError::TemplateNotFound(template_id.to_string()))
    }

    /// List fingerprints for a user
    pub fn list_fingerprints(&self, username: &str) -> Vec<FingerprintTemplate> {
        self.templates.lock().unwrap()
            .get(username)
            .cloned()
            .unwrap_or_default()
    }

    // ---- Authentication ----

    /// Start an authentication session (verify against stored template)
    pub fn start_auth(&self, username: &str, device_id: &str, is_identify: bool) -> Result<AuthSession, FprintdError> {
        if !self.is_running() {
            return Err(FprintdError::NotRunning);
        }
        let devices = self.devices.lock().unwrap();
        let device = devices.get(device_id)
            .ok_or_else(|| FprintdError::DeviceNotFound(device_id.to_string()))?;
        if device.state != DeviceState::Present {
            return Err(FprintdError::DeviceError);
        }
        drop(devices);

        // Check user has fingerprints
        let templates = self.templates.lock().unwrap();
        let has_templates = templates.get(username)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        drop(templates);

        if !has_templates && !is_identify {
            return Err(FprintdError::NoFingerprints(username.to_string()));
        }

        let session = AuthSession {
            id: format!("auth-{}-{}", username, now_ms()),
            username: username.to_string(),
            device_id: device_id.to_string(),
            is_identify,
            result: None,
            started_ms: now_ms(),
        };
        self.sessions.lock().unwrap().insert(session.id.clone(), session.clone());
        Ok(session)
    }

    /// Complete authentication with a result
    pub fn complete_auth(&self, session_id: &str, result: AuthResult) -> Result<(), FprintdError> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions.get_mut(session_id)
            .ok_or_else(|| FprintdError::SessionNotFound(session_id.to_string()))?;
        session.result = Some(result);
        Ok(())
    }

    /// Get session result
    pub fn get_auth_result(&self, session_id: &str) -> Option<AuthResult> {
        self.sessions.lock().unwrap()
            .get(session_id)
            .and_then(|s| s.result)
    }

    /// Cancel a session
    pub fn cancel_auth(&self, session_id: &str) -> Result<(), FprintdError> {
        self.complete_auth(session_id, AuthResult::Cancelled)
    }

    /// List all sessions
    pub fn list_sessions(&self) -> Vec<AuthSession> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }

    /// Check if user has any fingerprints enrolled
    pub fn has_fingerprints(&self, username: &str) -> bool {
        self.templates.lock().unwrap()
            .get(username)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }
}

impl Default for FingerprintService {
    fn default() -> Self { Self::new() }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[derive(Debug)]
pub enum FprintdError {
    NotRunning,
    DeviceNotFound(String),
    DeviceClaimed(String),
    DeviceNotClaimed,
    DeviceNotEnrollable,
    DeviceError,
    InvalidFinger(String),
    NoFingerprints(String),
    TemplateNotFound(String),
    SessionNotFound(String),
}

impl fmt::Display for FprintdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => write!(f, "service not running"),
            Self::DeviceNotFound(d) => write!(f, "device not found: {}", d),
            Self::DeviceClaimed(u) => write!(f, "device claimed by: {}", u),
            Self::DeviceNotClaimed => write!(f, "device not claimed by this user"),
            Self::DeviceNotEnrollable => write!(f, "device not enrollable"),
            Self::DeviceError => write!(f, "device error"),
            Self::InvalidFinger(fg) => write!(f, "invalid finger: {}", fg),
            Self::NoFingerprints(u) => write!(f, "no fingerprints for user: {}", u),
            Self::TemplateNotFound(t) => write!(f, "template not found: {}", t),
            Self::SessionNotFound(s) => write!(f, "session not found: {}", s),
        }
    }
}
impl std::error::Error for FprintdError {}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_device() -> FingerprintDevice {
        FingerprintDevice::new("usb:8087:0a2b", "Fingerprint Reader")
    }

    #[test]
    fn test_device_management() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        assert_eq!(svc.list_devices().len(), 1);
        assert!(svc.get_device("usb:8087:0a2b").is_some());
    }

    #[test]
    fn test_enroll_fingerprint() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        let t = svc.enroll("alice", "usb:8087:0a2b", "right-index-finger").unwrap();
        assert_eq!(t.username, "alice");
        assert_eq!(t.finger, "right-index-finger");
        assert!(svc.has_fingerprints("alice"));
    }

    #[test]
    fn test_invalid_finger() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        assert!(svc.enroll("alice", "usb:8087:0a2b", "big-toe").is_err());
    }

    #[test]
    fn test_device_claim() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        svc.claim_device("usb:8087:0a2b", "alice").unwrap();
        assert!(svc.claim_device("usb:8087:0a2b", "bob").is_err());
        svc.release_device("usb:8087:0a2b", "alice").unwrap();
        svc.claim_device("usb:8087:0a2b", "bob").unwrap();
    }

    #[test]
    fn test_auth_session() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        svc.enroll("alice", "usb:8087:0a2b", "right-index-finger").unwrap();

        let session = svc.start_auth("alice", "usb:8087:0a2b", false).unwrap();
        svc.complete_auth(&session.id, AuthResult::Success).unwrap();
        assert_eq!(svc.get_auth_result(&session.id), Some(AuthResult::Success));
    }

    #[test]
    fn test_no_fingerprints_auth() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        assert!(svc.start_auth("bob", "usb:8087:0a2b", false).is_err());
        // But identify (1:N) should work without user fingerprints
        assert!(svc.start_auth("bob", "usb:8087:0a2b", true).is_ok());
    }

    #[test]
    fn test_delete_template() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        let t = svc.enroll("alice", "usb:8087:0a2b", "left-thumb").unwrap();
        assert!(svc.has_fingerprints("alice"));
        svc.delete_template(&t.id).unwrap();
        assert!(!svc.has_fingerprints("alice"));
    }

    #[test]
    fn test_cancel_auth() {
        let svc = FingerprintService::new();
        svc.start();
        svc.add_device(test_device());
        svc.enroll("alice", "usb:8087:0a2b", "right-thumb").unwrap();
        let session = svc.start_auth("alice", "usb:8087:0a2b", false).unwrap();
        svc.cancel_auth(&session.id).unwrap();
        assert_eq!(svc.get_auth_result(&session.id), Some(AuthResult::Cancelled));
    }
}
