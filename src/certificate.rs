//! Certificate passthrough. The owner of the machine imports one PKCS#12
//! file and its password, and from then on every container this machine
//! starts gets both, read-only, at two fixed paths.
//!
//! This is sharing, not isolation. A container that can read the file and
//! the password can copy the private key. The owner turns it on knowing that.
//!
//! The imported file and the password are kept beside the config, mode 600,
//! never inside it. Each job gets its own copies in `secrets_dir`, which is
//! memory on a systemd machine and in WSL, and they are removed when the job
//! ends. Parsing is left to the `openssl` program: no key material is parsed
//! by this process and no crate is added for it.

use crate::config::Settings;
use crate::runtime::StartSpec;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where a container finds the certificate, whatever the file was called.
pub const CONTAINER_CERT: &str = "/run/shutup-and-run/certificate/client.pfx";
/// Where a container finds the password, as the only line of a file.
pub const CONTAINER_PASSWORD: &str = "/run/shutup-and-run/secrets/certificate-password";
/// Set in every container while passthrough is on. They hold paths.
pub const ENV_CERT: &str = "SHUTUP_AND_RUN_CERT_PATH";
pub const ENV_PASSWORD_FILE: &str = "SHUTUP_AND_RUN_CERT_PASSWORD_FILE";

/// A PKCS#12 file of a person or a company is a few kilobytes.
pub const MAX_PFX_BYTES: usize = 256 * 1024;

pub fn pfx_path(dir: &Path) -> PathBuf {
    dir.join("client.pfx")
}

pub fn password_path(dir: &Path) -> PathBuf {
    dir.join("password")
}

pub fn imported(dir: &Path) -> bool {
    pfx_path(dir).is_file() && password_path(dir).is_file()
}

/// Why a password cannot be stored: it travels as one line.
pub fn check_password(password: &str) -> Result<(), String> {
    if password.contains(['\n', '\r', '\0']) || password.len() > 1024 {
        return Err("the certificate password must be one line of at most 1024 bytes".into());
    }
    Ok(())
}

/// Keep the certificate and its password for later jobs. Both files are
/// replaced whole, mode 600. The original file is only ever read.
pub fn import(dir: &Path, pfx: &[u8], password: &str) -> Result<(), String> {
    check_password(password)?;
    if pfx.is_empty() || pfx.len() > MAX_PFX_BYTES {
        return Err(format!(
            "the certificate file must be between 1 byte and {} KiB",
            MAX_PFX_BYTES / 1024
        ));
    }
    let write = |path: PathBuf, bytes: &[u8]| {
        crate::config::write_private(&path, bytes)
            .map_err(|err| format!("could not write {}: {err}", path.display()))
    };
    write(pfx_path(dir), pfx)?;
    write(password_path(dir), password.as_bytes())?;
    restrict(dir, 0o700);
    Ok(())
}

/// Delete the imported certificate and password. Not a secure erase: the
/// blocks stay on the disk until something else is written over them.
pub fn forget(dir: &Path) {
    let _ = std::fs::remove_file(pfx_path(dir));
    let _ = std::fs::remove_file(password_path(dir));
    let _ = std::fs::remove_dir(dir);
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) {}

fn job_dir(settings: &Settings, id: &str) -> PathBuf {
    settings.secrets_dir.join(id)
}

/// Give one job its copies and point the container at them. Called only
/// when passthrough is on. An error here fails the job before it starts:
/// a job must not run without the certificate its owner expects it to have.
///
/// The copies are world-readable inside a directory only root can enter, so
/// a container that does not run as root can still read them through the
/// mount, and nobody else on the machine can.
pub fn stage(settings: &Settings, id: &str, spec: &mut StartSpec) -> Result<(), String> {
    let dir = job_dir(settings, id);
    let fail = |what: &str, err: std::io::Error| format!("certificate passthrough: {what}: {err}");
    std::fs::create_dir_all(&dir).map_err(|e| fail("could not make the job's folder", e))?;
    restrict(&settings.secrets_dir, 0o700);
    restrict(&dir, 0o700);
    for (from, name, inside) in [
        (
            pfx_path(&settings.certificate_dir),
            "client.pfx",
            CONTAINER_CERT,
        ),
        (
            password_path(&settings.certificate_dir),
            "certificate-password",
            CONTAINER_PASSWORD,
        ),
    ] {
        let to = dir.join(name);
        std::fs::copy(&from, &to).map_err(|e| {
            let _ = std::fs::remove_dir_all(&dir);
            fail("the imported certificate is missing or unreadable", e)
        })?;
        restrict(&to, 0o444);
        spec.binds.push(format!("{}:{inside}:ro", to.display()));
    }
    // A job cannot point its own code somewhere else by naming these two.
    spec.env.retain(|pair| {
        !pair.starts_with(&format!("{ENV_CERT}="))
            && !pair.starts_with(&format!("{ENV_PASSWORD_FILE}="))
    });
    spec.env.push(format!("{ENV_CERT}={CONTAINER_CERT}"));
    spec.env
        .push(format!("{ENV_PASSWORD_FILE}={CONTAINER_PASSWORD}"));
    Ok(())
}

/// Remove a job's copies. Safe to call for a job that never had any.
pub fn unstage(settings: &Settings, id: &str) {
    if crate::launcher::valid_container_id(id) {
        let _ = std::fs::remove_dir_all(job_dir(settings, id));
    }
}

/// After a restart: remove the copies of every job that is no longer running.
pub fn sweep(settings: &Settings, running: &HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(&settings.secrets_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !running.contains(&name) {
            unstage(settings, &name);
        }
    }
}

/// What a look at the certificate found. Nothing in it is secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Validation {
    pub ok: bool,
    /// `CERTIFICATE_VALID`, or what is wrong.
    pub code: &'static str,
    pub detail: String,
    pub fix: Option<String>,
    pub subject: Option<String>,
    pub not_before: Option<String>,
    pub not_after: Option<String>,
    pub has_private_key: bool,
}

impl Validation {
    fn bad(code: &'static str, detail: impl Into<String>, fix: &str) -> Self {
        Self {
            ok: false,
            code,
            detail: detail.into(),
            fix: Some(fix.to_string()),
            subject: None,
            not_before: None,
            not_after: None,
            has_private_key: false,
        }
    }
}

const SET_AGAIN: &str =
    "`shutup-and-run certificate set --file PATH` with the right file and password";

/// Check the file as a container would use it: it parses as PKCS#12 with
/// this password, holds a private key, and its certificate has not expired.
/// That is all it says. Whether a court's or a bank's system accepts the
/// certificate is decided there, not here.
pub fn validate(pfx: &Path, password: &str) -> Validation {
    if !pfx.is_file() {
        return Validation::bad(
            "CERTIFICATE_MISSING",
            "no certificate has been imported, or its file is gone",
            SET_AGAIN,
        );
    }
    let cert = match pkcs12(pfx, password, &["-clcerts", "-nokeys"]) {
        Ok(out) => out,
        Err(bad) => return *bad,
    };
    // The key is looked for and dropped at once. A file with no key in it
    // makes openssl fail here, which is the answer and not an error.
    let has_private_key = pkcs12(pfx, password, &["-nocerts", "-nodes"])
        .map(|key| key.contains("PRIVATE KEY-----"))
        .unwrap_or(false);
    // Without a key no certificate is "the client's": look at any of them,
    // so the answer below is "no private key" and not "no certificate".
    let cert = if cert.contains("-----BEGIN CERTIFICATE-----") {
        cert
    } else {
        pkcs12(pfx, password, &["-nokeys"]).unwrap_or_default()
    };
    if !cert.contains("-----BEGIN CERTIFICATE-----") {
        return Validation::bad(
            "CERTIFICATE_INVALID",
            "the file holds no certificate",
            SET_AGAIN,
        );
    }
    let x509 = openssl(
        &[
            "x509",
            "-noout",
            "-subject",
            "-startdate",
            "-enddate",
            "-checkend",
            "0",
        ],
        cert.as_bytes(),
    );
    let (expired, text) = match x509 {
        Ok((status, stdout, _)) => (!status, stdout),
        Err(err) => return Validation::bad("OPENSSL_MISSING", err, OPENSSL_FIX),
    };
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_string())
    };
    let mut found = Validation {
        ok: true,
        code: "CERTIFICATE_VALID",
        detail: "the file opens with this password, holds a private key, and has not expired"
            .to_string(),
        fix: None,
        subject: field("subject="),
        not_before: field("notBefore="),
        not_after: field("notAfter="),
        has_private_key,
    };
    if !has_private_key {
        found.ok = false;
        found.code = "CERTIFICATE_NO_PRIVATE_KEY";
        found.detail = "the file holds a certificate and no private key, so it cannot sign".into();
        found.fix = Some("export the certificate again with its private key".into());
    } else if expired {
        found.ok = false;
        found.code = "CERTIFICATE_EXPIRED";
        found.detail = format!(
            "the certificate expired on {}",
            found.not_after.as_deref().unwrap_or("an unknown date")
        );
        found.fix = Some("import a certificate that is still valid".into());
    }
    found
}

/// The certificate the machine has imported, checked with its stored password.
pub fn validate_imported(dir: &Path) -> Validation {
    let Ok(password) = std::fs::read_to_string(password_path(dir)) else {
        return Validation::bad(
            "CERTIFICATE_MISSING",
            "no certificate has been imported",
            SET_AGAIN,
        );
    };
    validate(&pfx_path(dir), &password)
}

const OPENSSL_FIX: &str = "install the openssl program where the service runs: \
     `apt-get install -y openssl` (on Windows: `wsl -d shutup-and-run -u root apt-get install -y openssl`)";

/// `openssl pkcs12` on the file, with the password on stdin so it is in no
/// argument list. Files written by older tools need the legacy provider.
fn pkcs12(pfx: &Path, password: &str, extra: &[&str]) -> Result<String, Box<Validation>> {
    let path = pfx.to_string_lossy();
    for legacy in [false, true] {
        let mut args = vec!["pkcs12", "-in", &path, "-passin", "stdin"];
        args.extend_from_slice(extra);
        if legacy {
            args.push("-legacy");
        }
        match openssl(&args, format!("{password}\n").as_bytes()) {
            Ok((true, stdout, _)) => return Ok(stdout),
            Ok((false, _, stderr)) => {
                let lower = stderr.to_ascii_lowercase();
                if lower.contains("invalid password") || lower.contains("mac verify") {
                    return Err(Box::new(Validation::bad(
                        "CERTIFICATE_PASSWORD_WRONG",
                        "the password does not open the certificate file",
                        SET_AGAIN,
                    )));
                }
            }
            Err(err) => {
                return Err(Box::new(Validation::bad(
                    "OPENSSL_MISSING",
                    err,
                    OPENSSL_FIX,
                )))
            }
        }
    }
    Err(Box::new(Validation::bad(
        "CERTIFICATE_INVALID",
        "the file is not a PKCS#12 (.pfx or .p12) file that openssl can read",
        SET_AGAIN,
    )))
}

/// Run openssl with `input` on stdin. (succeeded, stdout, stderr).
pub(crate) fn openssl(args: &[&str], input: &[u8]) -> Result<(bool, String, String), String> {
    let mut child = Command::new("openssl")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("the openssl program could not be run: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // openssl may stop reading early; that is its answer, not an error.
        let _ = stdin.write_all(input);
    }
    let out = child
        .wait_with_output()
        .map_err(|err| format!("openssl: {err}"))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

/// A throwaway certificate for the signing demo: self-signed, two days,
/// named so nobody mistakes it for a real one. Returns the PKCS#12 bytes.
pub fn demo_pfx(work: &Path, password: &str) -> Result<Vec<u8>, String> {
    let at = |name: &str| work.join(name).to_string_lossy().to_string();
    let run = |args: &[&str]| match openssl(args, b"") {
        Ok((true, _, _)) => Ok(()),
        Ok((false, _, stderr)) => Err(format!(
            "openssl could not make the demo certificate: {}",
            stderr.lines().last().unwrap_or("")
        )),
        Err(err) => Err(err),
    };
    run(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-days",
        "2",
        "-subj",
        "/CN=shutup-and-run demo (not a real certificate)/O=DEMO ONLY",
        "-keyout",
        &at("key.pem"),
        "-out",
        &at("cert.pem"),
    ])?;
    run(&[
        "pkcs12",
        "-export",
        "-inkey",
        &at("key.pem"),
        "-in",
        &at("cert.pem"),
        "-out",
        &at("demo.pfx"),
        "-passout",
        &format!("pass:{password}"),
    ])?;
    std::fs::read(work.join("demo.pfx")).map_err(|err| format!("demo certificate: {err}"))
}
