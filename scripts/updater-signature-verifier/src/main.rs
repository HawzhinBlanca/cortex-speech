//! Verify a Tauri v2 updater signature with the same decode/verification contract as the runtime.
//!
//! Tauri's `.sig` and configured updater public key are base64 wrappers around Minisign text.  A
//! filename match or a hash of the detached signature proves only that some bytes were retained;
//! this helper verifies that the artifact was signed by the exact key compiled into the app.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use minisign_verify::{PublicKey, Signature};
use std::{env, fs, path::PathBuf, process::ExitCode};

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("TAURI UPDATER SIGNATURE REJECTED: {message}");
    ExitCode::FAILURE
}

fn run() -> Result<(), String> {
    let mut args = env::args_os();
    let _program = args.next();
    let artifact = PathBuf::from(
        args.next()
            .ok_or("usage: verifier ARTIFACT SIGNATURE TAURI_BASE64_PUBLIC_KEY")?,
    );
    let signature_path = PathBuf::from(
        args.next()
            .ok_or("usage: verifier ARTIFACT SIGNATURE TAURI_BASE64_PUBLIC_KEY")?,
    );
    let public_key_arg = args
        .next()
        .ok_or("usage: verifier ARTIFACT SIGNATURE TAURI_BASE64_PUBLIC_KEY")?;
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let public_key_base64 = public_key_arg
        .to_str()
        .ok_or("updater public key is not valid UTF-8")?;
    let public_key_text = STANDARD
        .decode(public_key_base64)
        .map_err(|error| format!("updater public key is not canonical base64: {error}"))?;
    let public_key_text = std::str::from_utf8(&public_key_text)
        .map_err(|error| format!("decoded updater public key is not UTF-8: {error}"))?;
    let public_key = PublicKey::decode(public_key_text)
        .map_err(|error| format!("decoded updater public key is invalid: {error}"))?;

    let signature_base64 = fs::read_to_string(&signature_path)
        .map_err(|error| format!("cannot read {}: {error}", signature_path.display()))?;
    // Tauri writes the base64 wrapper as a text file. Whitespace would be accepted differently by
    // different decoders, so certification allows only the exact trimmed wrapper used at runtime.
    let signature_base64 = signature_base64.trim();
    if signature_base64.is_empty() || signature_base64.chars().any(char::is_whitespace) {
        return Err("updater signature wrapper is empty or contains whitespace".into());
    }
    let signature_text = STANDARD
        .decode(signature_base64)
        .map_err(|error| format!("updater signature is not canonical base64: {error}"))?;
    let signature_text = std::str::from_utf8(&signature_text)
        .map_err(|error| format!("decoded updater signature is not UTF-8: {error}"))?;
    let signature = Signature::decode(signature_text)
        .map_err(|error| format!("decoded updater signature is invalid: {error}"))?;

    let artifact_bytes = fs::read(&artifact)
        .map_err(|error| format!("cannot read {}: {error}", artifact.display()))?;
    public_key
        .verify(&artifact_bytes, &signature, true)
        .map_err(|error| format!("signature does not match artifact/public key: {error}"))?;
    println!("TAURI UPDATER SIGNATURE VERIFIED: {}", artifact.display());
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}
