#!/usr/bin/env python3
"""Validate one exact Windows release bundle and emit hash-bound artifact roles.

This module deliberately separates *measurement* from *claims*.  It never turns a JSON flag into
proof: Authenticode is re-verified with Windows' SignTool and the signer certificate is re-observed
with ``Get-AuthenticodeSignature``; GitHub/Sigstore provenance is re-verified with ``gh`` against an
on-disk bundle and an exact repository/workflow/source identity; checksums, SBOM metadata, the baked
Git SHA, and the build-environment record are derived again from the supplied bytes.

The safe updater default is explicit.  A bundle without the paired Tauri updater archive/signature
can be validated as a draft candidate, but ``--require-windows-product`` rejects it.  This script
does not enable an updater, invent a signature, or make a product-certification decision.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Iterable


CHECKSUM_NAME = "SHA256SUMS-windows-11-x64"
SBOM_NAME = "cortex-speech.cdx.json"
ENVIRONMENT_NAME = "release-environment.json"
PROVENANCE_NAME = "github-provenance.sigstore.json"
APPLICATION_NAME = "cortex-speech-app.exe"
SIGNER_WORKFLOW = ".github/workflows/release.yml"
READ_CHUNK_BYTES = 1024 * 1024
_CHECKSUM_LINE = re.compile(r"^([0-9a-f]{64}) [ *]([^\\\r\n]+)$")
_SHA40 = re.compile(r"^[0-9a-f]{40}$")
_SHA64 = re.compile(r"^[0-9a-f]{64}$")


class ReleaseBundleError(ValueError):
    """The supplied bytes do not satisfy the release-bundle authority contract."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(READ_CHUNK_BYTES):
            digest.update(chunk)
    return digest.hexdigest()


def _canonical_sha(value: str, label: str, length: int) -> str:
    normalized = value.strip().casefold()
    pattern = _SHA40 if length == 40 else _SHA64
    if not pattern.fullmatch(normalized):
        raise ReleaseBundleError(f"{label} must be exactly {length} lowercase hexadecimal characters")
    return normalized


def _canonical_fingerprint(value: str, label: str, length: int) -> str:
    normalized = re.sub(r"[^0-9A-Fa-f]", "", value).upper()
    if not re.fullmatch(rf"[0-9A-F]{{{length}}}", normalized):
        raise ReleaseBundleError(f"{label} must contain exactly {length} hexadecimal characters")
    return normalized


def _is_link_or_reparse(path: Path) -> bool:
    if path.is_symlink():
        return True
    attributes = getattr(path.lstat(), "st_file_attributes", 0)
    return bool(attributes & getattr(__import__("stat"), "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))


def _inventory(root: Path) -> dict[str, Path]:
    root = root.resolve(strict=True)
    if not root.is_dir() or _is_link_or_reparse(root):
        raise ReleaseBundleError("release bundle root must be a real directory, not a link/reparse point")
    inventory: dict[str, Path] = {}
    casefolded: set[str] = set()
    for candidate in sorted(root.rglob("*")):
        if _is_link_or_reparse(candidate):
            raise ReleaseBundleError(f"release bundle contains a link/reparse point: {candidate}")
        if not candidate.is_file():
            continue
        relative = candidate.relative_to(root).as_posix()
        if relative.startswith("/") or ".." in Path(relative).parts or relative in {"", "."}:
            raise ReleaseBundleError(f"release bundle contains an unsafe path: {relative!r}")
        folded = relative.casefold()
        if folded in casefolded:
            raise ReleaseBundleError(f"release bundle contains case-colliding paths: {relative}")
        inventory[relative] = candidate
        casefolded.add(folded)
    if not inventory:
        raise ReleaseBundleError("release bundle is empty")
    return inventory


def _exact_named(inventory: dict[str, Path], name: str, label: str) -> tuple[str, Path]:
    matches = [
        (relative, path)
        for relative, path in inventory.items()
        if Path(relative).name.casefold() == name.casefold()
    ]
    if len(matches) != 1:
        raise ReleaseBundleError(f"release bundle must contain exactly one {label}; found {len(matches)}")
    return matches[0]


def _exact_suffix(
    inventory: dict[str, Path],
    suffix: str,
    label: str,
    *,
    exclude: Iterable[str] = (),
) -> tuple[str, Path]:
    excluded = {value.casefold() for value in exclude}
    matches = [
        (relative, path)
        for relative, path in inventory.items()
        if Path(relative).name.casefold().endswith(suffix.casefold())
        and Path(relative).name.casefold() not in excluded
    ]
    if len(matches) != 1:
        raise ReleaseBundleError(f"release bundle must contain exactly one {label}; found {len(matches)}")
    return matches[0]


def _validate_checksums(root: Path, inventory: dict[str, Path]) -> tuple[str, Path]:
    relative, manifest_path = _exact_named(inventory, CHECKSUM_NAME, "checksum manifest")
    try:
        lines = manifest_path.read_text(encoding="ascii").splitlines()
    except (OSError, UnicodeError) as error:
        raise ReleaseBundleError(f"checksum manifest is not canonical ASCII: {error}") from error
    if not lines or any(not line for line in lines):
        raise ReleaseBundleError("checksum manifest is empty or contains blank lines")
    declared: dict[str, str] = {}
    folded: set[str] = set()
    for line in lines:
        match = _CHECKSUM_LINE.fullmatch(line)
        if not match:
            raise ReleaseBundleError(f"checksum manifest contains a malformed line: {line!r}")
        digest, declared_path = match.groups()
        parts = Path(declared_path).parts
        if declared_path.startswith("/") or ".." in parts or declared_path in {"", "."}:
            raise ReleaseBundleError(f"checksum manifest contains an unsafe path: {declared_path!r}")
        if declared_path.casefold() in folded:
            raise ReleaseBundleError(f"checksum manifest contains a duplicate/case-colliding path: {declared_path}")
        declared[declared_path] = digest
        folded.add(declared_path.casefold())
    expected = set(inventory) - {relative}
    if set(declared) != expected:
        missing = sorted(expected - set(declared))
        extra = sorted(set(declared) - expected)
        raise ReleaseBundleError(f"checksum inventory is not exact (missing={missing}, extra={extra})")
    for declared_path, expected_digest in declared.items():
        actual = sha256_file(root / declared_path)
        if actual != expected_digest:
            raise ReleaseBundleError(f"checksum mismatch for {declared_path}: {actual} != {expected_digest}")
    return relative, manifest_path


def _read_json(path: Path, label: str) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8-sig"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ReleaseBundleError(f"{label} is not readable canonical JSON: {error}") from error
    if not isinstance(value, dict):
        raise ReleaseBundleError(f"{label} must be a JSON object")
    return value


def _validate_environment(
    path: Path,
    *,
    expected_sha: str,
    expected_repository: str,
    expected_ref: str,
    repo_root: Path | None,
) -> None:
    value = _read_json(path, "release environment")
    source = value.get("source")
    runner = value.get("runner")
    tools = value.get("tools")
    inputs = value.get("inputs")
    if (
        value.get("schema") != 1
        or not isinstance(source, dict)
        or source.get("gitSha") != expected_sha
        or source.get("repository") != expected_repository
        or source.get("ref") != expected_ref
        or source.get("workflow") != f"{expected_repository}/{SIGNER_WORKFLOW}"
        or not isinstance(runner, dict)
        or runner.get("os") != "Windows"
        or str(runner.get("arch", "")).casefold() not in {"x64", "amd64"}
        or not isinstance(tools, dict)
        or any(
            not isinstance(tools.get(name), str) or not tools.get(name)
            for name in ("rustc", "cargo", "node", "npm", "python", "tauriCli")
        )
        or not isinstance(inputs, dict)
    ):
        raise ReleaseBundleError("release environment is not bound to the expected repository/ref/SHA/workflow")
    if repo_root is None:
        return
    authorities = {
        "packageLockSha256": repo_root / "cortex-speech-app" / "package-lock.json",
        "cargoLockSha256": repo_root / "cortex-speech-app" / "src-tauri" / "Cargo.lock",
        "releaseWorkflowSha256": repo_root / SIGNER_WORKFLOW,
    }
    for field, authority in authorities.items():
        if not authority.is_file() or inputs.get(field) != sha256_file(authority):
            raise ReleaseBundleError(f"release environment input {field} is missing or differs from committed bytes")


def _validate_sbom(path: Path, expected_version: str, repo_root: Path | None) -> None:
    value = _read_json(path, "CycloneDX SBOM")
    metadata = value.get("metadata")
    component = metadata.get("component") if isinstance(metadata, dict) else None
    components = value.get("components")
    if (
        value.get("bomFormat") != "CycloneDX"
        or value.get("specVersion") != "1.5"
        or value.get("version") != 1
        or not isinstance(component, dict)
        or component.get("type") != "application"
        or component.get("name") != "cortex-speech-app"
        or component.get("version") != expected_version
        or not isinstance(components, list)
        or not components
    ):
        raise ReleaseBundleError("CycloneDX SBOM metadata/version/dependency inventory is invalid")
    purls = [entry.get("purl") for entry in components if isinstance(entry, dict)]
    if len(purls) != len(components) or any(not isinstance(purl, str) or not purl for purl in purls):
        raise ReleaseBundleError("CycloneDX SBOM contains a component without a package URL")
    if purls != sorted(set(purls)):
        raise ReleaseBundleError("CycloneDX SBOM component inventory is not unique and deterministically sorted")
    if repo_root is not None:
        generator_path = repo_root / "cortex-speech-app" / "scripts" / "generate_sbom.py"
        spec = importlib.util.spec_from_file_location("cortex_release_sbom_generator", generator_path)
        if spec is None or spec.loader is None:
            raise ReleaseBundleError("cannot load the committed SBOM generator")
        generator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(generator)
        app_root = repo_root / "cortex-speech-app"
        expected = generator.build_sbom(
            app_root / "package-lock.json",
            app_root / "src-tauri" / "Cargo.lock",
            "cortex-speech-app",
            expected_version,
        )
        if value != expected:
            raise ReleaseBundleError("CycloneDX SBOM does not exactly match the committed lockfiles")


def _binary_marker(path: Path) -> str | None:
    tail = b""
    with path.open("rb") as handle:
        while chunk := handle.read(READ_CHUNK_BYTES):
            window = tail + chunk
            match = re.search(rb"CORTEX_BUILD_SHA:([0-9a-fA-F]{7,40}|unknown)", window)
            if match:
                return match.group(1).decode("ascii").casefold()
            tail = window[-96:]
    return None


def _signtool() -> Path:
    roots = [
        Path(value)
        for variable in ("ProgramFiles(x86)", "ProgramFiles")
        if (value := os.environ.get(variable))
    ]
    matches: list[Path] = []
    for root in roots:
        matches.extend((root / "Windows Kits" / "10" / "bin").glob("*/x64/signtool.exe"))
    matches = sorted({path.resolve() for path in matches if path.is_file()}, key=lambda path: str(path))
    if not matches:
        raise ReleaseBundleError("signtool.exe was not found; Authenticode cannot be independently verified")
    return matches[-1]


def _powershell_executable() -> str:
    for candidate in ("pwsh.exe", "powershell.exe", "pwsh", "powershell"):
        resolved = shutil.which(candidate)
        if resolved:
            return resolved
    raise ReleaseBundleError("PowerShell was not found; signer certificate identity cannot be observed")


def _verify_authenticode(path: Path, expected_thumbprint: str, expected_cert_sha256: str) -> dict[str, object]:
    if os.name != "nt":
        raise ReleaseBundleError("Authenticode verification requires a Windows runner")
    tool = _signtool()
    completed = subprocess.run(
        [str(tool), "verify", "/pa", "/all", "/v", "/tw", str(path)],
        capture_output=True,
        text=True,
        check=False,
        timeout=120,
        shell=False,
    )
    if completed.returncode != 0:
        detail = ((completed.stdout or "") + "\n" + (completed.stderr or "")).strip()[-2000:]
        raise ReleaseBundleError(f"Authenticode/timestamp verification failed for {path.name}: {detail}")
    script = (
        "$ErrorActionPreference='Stop';"
        "$s=Get-AuthenticodeSignature -LiteralPath $args[0];"
        "if($null -eq $s.SignerCertificate){throw 'missing signer certificate'};"
        "$sha=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($s.SignerCertificate.RawData));"
        "[ordered]@{status=[string]$s.Status;thumbprint=[string]$s.SignerCertificate.Thumbprint;"
        "certificateSha256=$sha;timestampSubject=if($s.TimeStamperCertificate)"
        "{[string]$s.TimeStamperCertificate.Subject}else{$null}}"
        "|ConvertTo-Json -Compress"
    )
    observed = subprocess.run(
        [_powershell_executable(), "-NoProfile", "-NonInteractive", "-Command", script, str(path)],
        capture_output=True,
        text=True,
        check=False,
        timeout=60,
        shell=False,
    )
    if observed.returncode != 0:
        raise ReleaseBundleError(
            f"cannot observe Authenticode signer identity for {path.name}: {observed.stderr[-2000:]}"
        )
    try:
        identity = json.loads(observed.stdout)
    except json.JSONDecodeError as error:
        raise ReleaseBundleError(f"PowerShell returned malformed signer identity for {path.name}") from error
    thumbprint = _canonical_fingerprint(str(identity.get("thumbprint", "")), "observed signer thumbprint", 40)
    cert_sha256 = _canonical_fingerprint(str(identity.get("certificateSha256", "")), "observed signer SHA-256", 64)
    if (
        identity.get("status") != "Valid"
        or thumbprint != expected_thumbprint
        or cert_sha256 != expected_cert_sha256
        or not identity.get("timestampSubject")
    ):
        raise ReleaseBundleError(f"unexpected/invalid signer or missing timestamp authority for {path.name}")
    return {
        "status": "Valid",
        "signerThumbprintSha1": thumbprint,
        "signerCertificateSha256": cert_sha256,
        "timestampVerified": True,
        "timestampAuthoritySubject": str(identity["timestampSubject"])[:512],
    }


def _verify_provenance(
    subjects: list[Path],
    bundle: Path,
    *,
    repository: str,
    source_sha: str,
    source_ref: str,
) -> None:
    gh = shutil.which("gh")
    if gh is None:
        raise ReleaseBundleError("GitHub CLI was not found; Sigstore provenance cannot be verified")
    signer_workflow = f"{repository}/{SIGNER_WORKFLOW}"
    for subject in subjects:
        completed = subprocess.run(
            [
                gh,
                "attestation",
                "verify",
                str(subject),
                "--bundle",
                str(bundle),
                "--repo",
                repository,
                "--signer-workflow",
                signer_workflow,
                "--source-digest",
                source_sha,
                "--source-ref",
                source_ref,
                "--deny-self-hosted-runners",
            ],
            capture_output=True,
            text=True,
            check=False,
            timeout=120,
            shell=False,
        )
        if completed.returncode != 0:
            detail = ((completed.stdout or "") + "\n" + (completed.stderr or "")).strip()[-2000:]
            raise ReleaseBundleError(f"GitHub/Sigstore provenance verification failed for {subject.name}: {detail}")


def _configured_updater_public_key(repo_root: Path) -> str:
    """Return the exact updater key compiled into this source, or fail closed.

    An external secret/status value is not an updater authority: the shipped runtime verifies with
    the public key in ``tauri.conf.json``.  Certification therefore uses those committed bytes and
    also refuses an updater configuration that enables insecure transport or has no HTTPS endpoint.
    """

    config_path = repo_root / "cortex-speech-app" / "src-tauri" / "tauri.conf.json"
    config = _read_json(config_path, "Tauri updater configuration")
    bundle = config.get("bundle")
    plugins = config.get("plugins")
    updater = plugins.get("updater") if isinstance(plugins, dict) else None
    public_key = updater.get("pubkey") if isinstance(updater, dict) else None
    endpoints = updater.get("endpoints") if isinstance(updater, dict) else None
    if (
        not isinstance(bundle, dict)
        or bundle.get("createUpdaterArtifacts") is not True
        or not isinstance(public_key, str)
        or not public_key
        or not isinstance(endpoints, list)
        or not endpoints
        or any(not isinstance(endpoint, str) or not endpoint.startswith("https://") for endpoint in endpoints)
        or updater.get("dangerousInsecureTransportProtocol") is True
    ):
        raise ReleaseBundleError(
            "updater artifacts require a committed key, HTTPS endpoint, secure transport, and createUpdaterArtifacts=true"
        )
    try:
        decoded = base64.b64decode(public_key, validate=True).decode("utf-8")
    except (ValueError, UnicodeError) as error:
        raise ReleaseBundleError("committed Tauri updater public key is not canonical base64/UTF-8") from error
    if "untrusted comment:" not in decoded or len(decoded) > 4096:
        raise ReleaseBundleError("committed Tauri updater public key has an invalid Minisign envelope")
    return public_key


def _updater_verifier_source_identity(repo_root: Path) -> str:
    verifier_root = repo_root / "scripts" / "updater-signature-verifier"
    authorities = [
        verifier_root / "Cargo.toml",
        verifier_root / "Cargo.lock",
        verifier_root / "src" / "main.rs",
    ]
    digest = hashlib.sha256()
    for authority in authorities:
        if not authority.is_file():
            raise ReleaseBundleError(f"updater verifier source authority is missing: {authority}")
        relative = authority.relative_to(repo_root).as_posix().encode("utf-8")
        blob = authority.read_bytes()
        digest.update(len(relative).to_bytes(4, "big"))
        digest.update(relative)
        digest.update(len(blob).to_bytes(8, "big"))
        digest.update(blob)
    return digest.hexdigest()


def _verify_updater_signature(
    archive: Path,
    signature: Path,
    *,
    repo_root: Path,
) -> dict[str, object]:
    """Verify exactly as Tauri v2 does: base64 wrappers + ``minisign-verify`` strict mode."""

    public_key = _configured_updater_public_key(repo_root)
    manifest = repo_root / "scripts" / "updater-signature-verifier" / "Cargo.toml"
    cargo = shutil.which("cargo")
    if cargo is None:
        raise ReleaseBundleError("Cargo was not found; Tauri updater signature cannot be verified")
    completed = subprocess.run(
        [
            cargo,
            "run",
            "--quiet",
            "--locked",
            "--release",
            "--manifest-path",
            str(manifest),
            "--",
            str(archive),
            str(signature),
            public_key,
        ],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
        timeout=600,
        shell=False,
    )
    if completed.returncode != 0:
        detail = ((completed.stdout or "") + "\n" + (completed.stderr or "")).strip()[-2000:]
        raise ReleaseBundleError(f"Tauri updater signature verification failed: {detail}")
    return {
        "verified": True,
        "publicKeySha256": hashlib.sha256(public_key.encode("utf-8")).hexdigest(),
        "verifierSourceSha256": _updater_verifier_source_identity(repo_root),
        "verifier": "minisign-verify-0.2.5/tauri-v2-strict",
    }


def _artifact(role: str, relative: str, path: Path, **extra: object) -> dict[str, object]:
    return {
        "role": role,
        "name": path.name,
        "path": relative,
        "sha256": sha256_file(path),
        "bytes": path.stat().st_size,
        **extra,
    }


def validate_bundle(
    bundle_dir: Path,
    *,
    expected_sha: str,
    expected_repository: str,
    expected_ref: str,
    expected_version: str,
    signer_thumbprint: str,
    signer_cert_sha256: str,
    repo_root: Path | None = None,
    verify_authenticode: bool = False,
    verify_provenance: bool = False,
    require_windows_product: bool = False,
) -> dict[str, object]:
    root = bundle_dir.resolve(strict=True)
    resolved_repo_root = repo_root.resolve(strict=True) if repo_root is not None else None
    expected_sha = _canonical_sha(expected_sha, "expected Git SHA", 40)
    expected_thumbprint = _canonical_fingerprint(signer_thumbprint, "signer thumbprint", 40)
    expected_cert_sha256 = _canonical_fingerprint(signer_cert_sha256, "signer certificate SHA-256", 64)
    if not expected_repository or expected_repository.count("/") != 1:
        raise ReleaseBundleError("expected repository must be an exact owner/name identity")
    if not expected_ref.startswith("refs/tags/v"):
        raise ReleaseBundleError("expected release ref must be a stable tag ref")
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", expected_version):
        raise ReleaseBundleError("expected version must be stable SemVer without a prefix")
    if expected_ref != f"refs/tags/v{expected_version}":
        raise ReleaseBundleError("release ref and package version do not name the same stable release")

    inventory = _inventory(root)
    checksum_relative, checksum_path = _validate_checksums(root, inventory)
    app_relative, app_path = _exact_named(inventory, APPLICATION_NAME, "application executable")
    msi_relative, msi_path = _exact_suffix(inventory, ".msi", "MSI installer")
    nsis_relative, nsis_path = _exact_suffix(
        inventory, "-setup.exe", "NSIS installer", exclude=(APPLICATION_NAME,)
    )
    sbom_relative, sbom_path = _exact_named(inventory, SBOM_NAME, "CycloneDX SBOM")
    environment_relative, environment_path = _exact_named(inventory, ENVIRONMENT_NAME, "release environment")
    provenance_relative, provenance_path = _exact_named(
        inventory, PROVENANCE_NAME, "GitHub/Sigstore provenance bundle"
    )

    marker = _binary_marker(app_path)
    if marker != expected_sha:
        raise ReleaseBundleError(f"application executable carries build SHA {marker!r}, expected {expected_sha}")
    _validate_environment(
        environment_path,
        expected_sha=expected_sha,
        expected_repository=expected_repository,
        expected_ref=expected_ref,
        repo_root=resolved_repo_root,
    )
    _validate_sbom(sbom_path, expected_version, resolved_repo_root)

    updater_archives = [
        (relative, path)
        for relative, path in inventory.items()
        if relative.casefold().endswith((".nsis.zip", ".msi.zip"))
    ]
    updater_signatures = [
        (relative, path)
        for relative, path in inventory.items()
        if relative.casefold().endswith((".nsis.zip.sig", ".msi.zip.sig"))
    ]
    if len(updater_archives) > 1 or len(updater_signatures) > 1 or bool(updater_archives) != bool(updater_signatures):
        raise ReleaseBundleError("Tauri updater authority must be exactly one archive plus its detached signature")
    if updater_archives and updater_signatures:
        if updater_signatures[0][1].name != updater_archives[0][1].name + ".sig":
            raise ReleaseBundleError("Tauri updater detached signature does not match its archive name")
        if resolved_repo_root is None:
            raise ReleaseBundleError(
                "Tauri updater signature verification requires the exact committed source/configuration"
            )

    allowed = {
        checksum_relative,
        app_relative,
        msi_relative,
        nsis_relative,
        sbom_relative,
        environment_relative,
        provenance_relative,
        *(relative for relative, _ in updater_archives),
        *(relative for relative, _ in updater_signatures),
    }
    unexpected = sorted(set(inventory) - allowed)
    if unexpected:
        raise ReleaseBundleError(f"release bundle contains undeclared files: {unexpected}")

    auth: dict[str, dict[str, object]] = {}
    if verify_authenticode:
        for role, path in (
            ("application-executable", app_path),
            ("windows-msi", msi_path),
            ("windows-nsis", nsis_path),
        ):
            auth[role] = _verify_authenticode(path, expected_thumbprint, expected_cert_sha256)
    if verify_provenance:
        _verify_provenance(
            [
                app_path,
                msi_path,
                nsis_path,
                sbom_path,
                environment_path,
                *(path for _, path in updater_archives),
                *(path for _, path in updater_signatures),
            ],
            provenance_path,
            repository=expected_repository,
            source_sha=expected_sha,
            source_ref=expected_ref,
        )

    updater_signature_authority: dict[str, object] | None = None
    if updater_archives and updater_signatures:
        updater_signature_authority = _verify_updater_signature(
            updater_archives[0][1],
            updater_signatures[0][1],
            repo_root=resolved_repo_root,
        )

    artifacts = [
        _artifact(
            "application-executable",
            app_relative,
            app_path,
            buildGitSha=marker,
            matchesFullGitSha=True,
            authenticode=auth.get("application-executable"),
        ),
        _artifact("windows-msi", msi_relative, msi_path, authenticode=auth.get("windows-msi")),
        _artifact("windows-nsis", nsis_relative, nsis_path, authenticode=auth.get("windows-nsis")),
        _artifact("release-checksums", checksum_relative, checksum_path, inventoryVerified=True),
        _artifact("cyclonedx-sbom", sbom_relative, sbom_path, sbomVerified=True),
        _artifact(
            "github-sigstore-provenance",
            provenance_relative,
            provenance_path,
            provenanceVerified=verify_provenance,
            signerWorkflow=f"{expected_repository}/{SIGNER_WORKFLOW}",
            sourceGitSha=expected_sha,
            sourceRef=expected_ref,
        ),
        _artifact("release-environment", environment_relative, environment_path, sourceBound=True),
    ]
    if updater_archives:
        archive_relative, archive_path = updater_archives[0]
        signature_relative, signature_path = updater_signatures[0]
        artifacts.append(
            _artifact(
                "windows-updater",
                archive_relative,
                archive_path,
                signature={
                    "name": signature_path.name,
                    "path": signature_relative,
                    "sha256": sha256_file(signature_path),
                    "bytes": signature_path.stat().st_size,
                    **(updater_signature_authority or {}),
                },
            )
        )

    certification_ready = bool(
        verify_authenticode
        and verify_provenance
        and updater_archives
        and updater_signatures
        and updater_signature_authority
    )
    if require_windows_product and not certification_ready:
        missing = []
        if not verify_authenticode:
            missing.append("independent Authenticode verification")
        if not verify_provenance:
            missing.append("independent GitHub/Sigstore verification")
        if not updater_archives:
            missing.append("Tauri updater archive/signature")
        elif updater_signature_authority is None:
            missing.append("cryptographically verified Tauri updater signature")
        raise ReleaseBundleError("Windows product release bundle is incomplete: " + ", ".join(missing))

    return {
        "schema": 1,
        "type": "WindowsReleaseBundleAuthorityV1",
        "source": {
            "fullGitSha": expected_sha,
            "repository": expected_repository,
            "ref": expected_ref,
            "version": expected_version,
        },
        "signer": {
            "thumbprintSha1": expected_thumbprint,
            "certificateSha256": expected_cert_sha256,
        },
        "cryptographicValidation": {
            "authenticodeAndTimestampVerified": verify_authenticode,
            "githubSigstoreProvenanceVerified": verify_provenance,
        },
        "certificationReady": certification_ready,
        "artifacts": artifacts,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--expected-sha", required=True)
    parser.add_argument("--expected-repository", required=True)
    parser.add_argument("--expected-ref", required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--signer-thumbprint", required=True)
    parser.add_argument("--signer-cert-sha256", required=True)
    parser.add_argument("--repo-root", type=Path)
    parser.add_argument("--verify-authenticode", action="store_true")
    parser.add_argument("--verify-provenance", action="store_true")
    parser.add_argument("--require-windows-product", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        authority = validate_bundle(
            args.bundle,
            expected_sha=args.expected_sha,
            expected_repository=args.expected_repository,
            expected_ref=args.expected_ref,
            expected_version=args.expected_version,
            signer_thumbprint=args.signer_thumbprint,
            signer_cert_sha256=args.signer_cert_sha256,
            repo_root=args.repo_root,
            verify_authenticode=args.verify_authenticode,
            verify_provenance=args.verify_provenance,
            require_windows_product=args.require_windows_product,
        )
    except (OSError, ReleaseBundleError, subprocess.SubprocessError) as error:
        print(f"WINDOWS RELEASE BUNDLE REJECTED: {error}", file=sys.stderr, flush=True)
        return 1
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(authority, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    state = "CERTIFICATION-READY" if authority["certificationReady"] else "DRAFT-ONLY"
    print(f"WINDOWS RELEASE BUNDLE {state}: {args.output}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
