#!/usr/bin/env python3
"""Bundle, release-helper, and recovered-attempt validation for owner-proof inputs.

The small runtime parameter keeps this policy module independent from the CLI/orchestration facade
while preserving its test seams.  Every callback is supplied by the facade that already owns the
relevant path, build, hashing, and platform authorities.
"""

from __future__ import annotations

import hashlib
import os
import re
import stat
import uuid
from pathlib import Path
from typing import Any, Mapping


def manifest_files(api: Any, manifest: Mapping[str, Any]) -> dict[str, dict[str, Any]]:
    files = manifest.get("files")
    if not isinstance(files, list):
        raise api.ProofInputError("manifest files must be a list")
    by_role: dict[str, dict[str, Any]] = {}
    paths: set[str] = set()
    for item in files:
        if not isinstance(item, dict):
            raise api.ProofInputError("manifest file entry must be an object")
        api._expect_keys(
            item,
            {"role", "relativePath", "sha256", "sizeBytes", "readOnlyHashBound"},
            context="manifest file",
        )
        role = item["role"]
        if not isinstance(role, str) or not role or role in by_role:
            raise api.ProofInputError("manifest file roles must be unique strings")
        relative = api._relative_path(item["relativePath"], context=f"manifest {role} path")
        if relative in paths:
            raise api.ProofInputError("manifest file paths must be unique")
        api._lower_sha256(item["sha256"], context=f"manifest {role} sha256")
        api._positive_int(item["sizeBytes"], context=f"manifest {role} size")
        if item["readOnlyHashBound"] is not True:
            raise api.ProofInputError("every baseline manifest file must be read-only and hash-bound")
        by_role[role] = item
        paths.add(relative)
    return by_role


def assert_manifest_has_no_private_paths(api: Any, value: Any) -> None:
    forbidden_keys = {"sourcepath", "sourcedirectory", "originalpath", "absolutepath"}
    if isinstance(value, dict):
        for key, child in value.items():
            if str(key).casefold() in forbidden_keys:
                raise api.ProofInputError("manifest persists a private source-path field")
            api._assert_manifest_has_no_private_paths(child)
    elif isinstance(value, list):
        for child in value:
            api._assert_manifest_has_no_private_paths(child)
    elif isinstance(value, str):
        if re.match(r"^[A-Za-z]:[\\/]", value) or value.startswith("\\\\") or value.startswith("//"):
            raise api.ProofInputError("manifest persists an absolute private path")


def assert_bundle_inventory(api: Any, root: Path, expected_files: set[str]) -> None:
    allowed_top = {
        api.MANIFEST_NAME,
        api.CONTRACT_BUNDLE_PATH,
        "media",
        "audiobook",
        "db-authorities",
        "db-derived",
        "tools",
        api.ATTEMPTS_DIR,
    }
    attempts = root / api.ATTEMPTS_DIR
    try:
        attempts_metadata = os.lstat(attempts)
    except OSError as error:
        raise api.ProofInputError("bundle must contain exactly one direct attempts directory") from error
    if (
        stat.S_ISLNK(attempts_metadata.st_mode)
        or api._metadata_reparse(attempts_metadata)
        or not stat.S_ISDIR(attempts_metadata.st_mode)
    ):
        raise api.ProofInputError("bundle attempts entry must be one direct non-reparse directory")
    for child in root.iterdir():
        metadata = os.lstat(child)
        if stat.S_ISLNK(metadata.st_mode) or api._metadata_reparse(metadata):
            raise api.ProofInputError("bundle contains a symlink or reparse point")
        if child.name not in allowed_top:
            raise api.ProofInputError("bundle contains an undeclared top-level entry")
        if not stat.S_ISREG(metadata.st_mode) and not stat.S_ISDIR(metadata.st_mode):
            raise api.ProofInputError("bundle contains a non-file, non-directory object")
    observed: set[str] = set()
    for path in root.rglob("*"):
        relative = path.relative_to(root).as_posix()
        if relative == api.ATTEMPTS_DIR or relative.startswith(f"{api.ATTEMPTS_DIR}/"):
            metadata = os.lstat(path)
            if stat.S_ISLNK(metadata.st_mode) or api._metadata_reparse(metadata):
                raise api.ProofInputError("attempt tree contains a symlink or reparse point")
            if not stat.S_ISREG(metadata.st_mode) and not stat.S_ISDIR(metadata.st_mode):
                raise api.ProofInputError("attempt tree contains a non-file, non-directory object")
            continue
        metadata = os.lstat(path)
        if stat.S_ISLNK(metadata.st_mode) or api._metadata_reparse(metadata):
            raise api.ProofInputError("bundle contains a symlink or reparse point")
        if stat.S_ISREG(metadata.st_mode):
            observed.add(relative)
        elif not stat.S_ISDIR(metadata.st_mode):
            raise api.ProofInputError("bundle contains a non-file, non-directory object")
    if observed != expected_files | {api.MANIFEST_NAME}:
        raise api.ProofInputError(
            f"bundle file inventory differs from its manifest: missing={sorted(expected_files - observed)}, "
            f"unknown={sorted(observed - expected_files - {api.MANIFEST_NAME})}"
        )


def proof_container(api: Any, bundle: Path) -> tuple[Path, Path]:
    if bundle.name != api.BUNDLE_DIR:
        raise api.ProofInputError(
            f"proof bundle must be the fixed {api.BUNDLE_DIR} child of its publication container"
        )
    container = api._assert_no_links(bundle.parent)
    verify_root = api._assert_no_links(container / api.VERIFY_ROOT_DIR)
    if not container.is_dir() or not verify_root.is_dir():
        raise api.ProofInputError("proof publication container is incomplete")
    with os.scandir(container) as scanned:
        entries = {entry.name for entry in scanned}
    required = {api.BUNDLE_DIR, api.VERIFY_ROOT_DIR}
    if not required.issubset(entries) or entries - required - set(api.PUBLISHED_TRANSACTION_FILES):
        raise api.ProofInputError("proof publication container inventory is not exact")
    return container, verify_root


def verify_release_helper_rebuild(
    api: Any,
    bundle: Path,
    contract: Mapping[str, Any],
    manifest: Mapping[str, Any],
    helper_entry: Mapping[str, Any],
    release_git: Path,
) -> None:
    release_sha = str(manifest["releaseGitSha"])
    if api._git_sha_clean(release_git) != release_sha:
        raise api.ProofInputError("release helper validation requires the exact clean release checkout")
    _container, parent = api._proof_container(bundle)
    parent_lock = api._OwnedDirectoryLock(parent, pin_namespace=False)
    mutex = None
    manifest_sha256 = hashlib.sha256(api.canonical_json_bytes(manifest)).hexdigest()
    workspace_key = hashlib.sha256(f"{release_sha}\n{manifest_sha256}".encode("ascii")).hexdigest()
    workspace = parent / f"{api.VERIFY_BUILD_PREFIX}{workspace_key[:32]}"
    workspace_lock = None
    workspace_identity: tuple[int, int] | None = None
    binary_lock = None
    try:
        mutex = api.NamedMutex("CortexOwnerProofVerifyBuild", api._normalized_path(bundle))
        with os.scandir(parent) as entries:
            names = {entry.name for entry in entries}
        unknown = names - {workspace.name}
        if unknown:
            raise api.ProofInputError("verification workspace root contains an unknown entry")
        if workspace.name in names:
            stale_lock = api._OwnedDirectoryLock(workspace)
            stale_identity = stale_lock.identity
            stale_lock.close()
            api._remove_owned_staging(workspace, parent, api.VERIFY_BUILD_PREFIX, stale_identity)
            if os.path.lexists(workspace):
                raise api.ProofInputError("stale verification workspace could not be removed safely")
        workspace.mkdir(mode=0o700)
        workspace_lock = api._OwnedDirectoryLock(workspace)
        workspace_identity = workspace_lock.identity
        lease = {
            "schema": 1,
            "workspaceKey": workspace_key,
            "releaseGitSha": release_sha,
            "bundleManifestSha256": manifest_sha256,
            "workspaceIdentity": list(workspace_identity),
        }
        lease_path = workspace / api.VERIFY_LEASE_NAME
        descriptor = os.open(
            lease_path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0),
            0o600,
        )
        with os.fdopen(descriptor, "wb", closefd=True) as target:
            target.write(api.canonical_json_bytes(lease))
            target.flush()
            os.fsync(target.fileno())
        api._fsync_directory(workspace)
        (
            _binary,
            binary_lock,
            rebuilt_hash,
            rebuilt_size,
            build_root,
            _build_identity,
            build_evidence,
        ) = api._build_release_helper(workspace, release_sha, contract["helperToolchain"], release_git)
        if not api._is_within(build_root, workspace):
            raise api.ProofInputError("independent helper build escaped its owned verification workspace")
        binary_lock.verify()
        if build_evidence != manifest["helperBuild"]:
            raise api.ProofInputError("independent helper build evidence differs from the bundle manifest")
        if rebuilt_hash != helper_entry["sha256"] or rebuilt_size != helper_entry["sizeBytes"]:
            raise api.ProofInputError("bundled helper differs from the independently rebuilt release helper")
    finally:
        if binary_lock is not None:
            binary_lock.close()
        if workspace_lock is not None:
            workspace_lock.close()
        try:
            if workspace_identity is not None:
                api._remove_owned_staging(workspace, parent, api.VERIFY_BUILD_PREFIX, workspace_identity)
                if os.path.lexists(workspace):
                    raise api.ProofInputError("owned helper-verification workspace could not be removed safely")
        finally:
            if mutex is not None:
                mutex.close()
            parent_lock.close()


def require_directory_denials(api: Any, path: Path, rights: tuple[int, ...], *, context: str) -> None:
    for right in rights:
        if not api._access_is_denied(path, right, directory=True):
            raise api.ProofInputError(f"{context} lacks required namespace denial 0x{right:x}")


def require_container_namespace_seals(api: Any, container: Path, bundle: Path, verify_root: Path) -> None:
    api._require_directory_denials(container, (0x2, 0x4, 0x40), context="published proof container")
    api._require_directory_denials(bundle, (0x00010000,), context="published bundle root")
    api._require_directory_denials(
        verify_root,
        (0x00010000, 0x2, 0x40),
        context="published verification workspace root",
    )
    if api._access_is_denied(verify_root, 0x4, directory=True):
        raise api.ProofInputError("published verification workspace cannot create an owned workspace directory")


def require_bundle_namespace_seals(api: Any, bundle: Path) -> None:
    api._require_directory_denials(
        bundle,
        (0x00010000, 0x2, 0x4, 0x40),
        context="published bundle root",
    )
    for name in ("media", "audiobook", "db-authorities", "db-derived", "tools", api.ATTEMPTS_DIR):
        child = bundle / name
        api._require_directory_denials(child, (0x00010000,), context="published bundle child")
        if name == api.ATTEMPTS_DIR:
            api._require_directory_denials(child, (0x2, 0x40), context="published attempts container")
            if api._access_is_denied(child, 0x4, directory=True):
                raise api.ProofInputError("published attempts container cannot create a fresh attempt directory")
        else:
            api._require_directory_denials(child, (0x2, 0x4, 0x40), context="published bundle content")


def validate_bundle(
    api: Any,
    root: Path,
    *,
    helper_factory: Any,
    allow_staging: bool,
    expected_contract_sha256: str,
) -> dict[str, Any]:
    bundle = api._assert_no_links(root)
    container, verify_root = api._proof_container(bundle)
    locks: list[Any] = []
    try:
        locks.append(api._OwnedDirectoryLock(container.parent))
        locks.append(api._OwnedDirectoryLock(container))
        locks.append(api._OwnedDirectoryLock(bundle))
        locks.append(api._OwnedDirectoryLock(verify_root, pin_namespace=False))
        transaction_owner = api._validate_published_transaction(
            container,
            kind="prepare",
            normalized_final_path=api._normalized_path(container),
            release_git_sha=None,
            run_token=None,
            mutable_descendant_roots=(api.VERIFY_ROOT_DIR, f"{api.BUNDLE_DIR}/{api.ATTEMPTS_DIR}"),
        )
        api._require_container_namespace_seals(container, bundle, verify_root)
        api._require_bundle_namespace_seals(bundle)
        for name in ("media", "audiobook", "db-authorities", "db-derived", "tools", api.ATTEMPTS_DIR):
            locks.append(api._OwnedDirectoryLock(bundle / name, pin_namespace=False))
        manifest = api._validate_bundle_locked(
            bundle,
            helper_factory=helper_factory,
            allow_staging=allow_staging,
            expected_contract_sha256=expected_contract_sha256,
            require_namespace_seals=True,
        )
        if transaction_owner["releaseGitSha"] != manifest["releaseGitSha"]:
            raise api.ProofInputError("publication transaction and bundle manifest release identities differ")
        if not allow_staging:
            # A previous publication may have reached its final name but failed
            # the post-rename parent flush.  Accept only after every content and
            # transaction check passes and a later flush makes the final namespace
            # durable while its parent identity remains locked.
            api._fsync_directory(container.parent)
        return manifest
    finally:
        for locked in reversed(locks):
            locked.close()


def validate_bundle_locked(
    api: Any,
    root: Path,
    *,
    helper_factory: Any,
    allow_staging: bool,
    expected_contract_sha256: str,
    require_namespace_seals: bool,
) -> dict[str, Any]:
    release_contract = expected_contract_sha256 == api.RELEASE_CONTRACT_SHA256
    if release_contract and (helper_factory is not api._default_helper_factory or os.name != "nt"):
        raise api.ProofInputError("release proof validation requires Windows and the real hash-bound helper")
    bundle = api._assert_no_links(root)
    if not bundle.is_dir():
        raise api.ProofInputError("proof bundle must be a directory")
    api._reject_protected(bundle)
    if api._is_within(bundle, api.REPO_ROOT):
        raise api.ProofInputError("proof bundles remain invalid after being moved inside the Git worktree")
    if api._is_snapshot_path(bundle):
        raise api.ProofInputError("snapshot recovery trees cannot be proof bundles")
    if not allow_staging and bundle.name.startswith(api.STAGING_PREFIX):
        raise api.ProofInputError("an unpublished staging tree is not a proof bundle")
    manifest_path = bundle / api.MANIFEST_NAME
    if require_namespace_seals and not api._access_is_denied(manifest_path, 0x00010000, directory=False):
        raise api.ProofInputError("published manifest lacks its deletion seal")
    manifest = api._load_json(manifest_path, canonical=True)
    api._expect_keys(
        manifest,
        {
            "schema",
            "bundleId",
            "releaseGitSha",
            "contractSha256",
            "helperSha256",
            "helperSourceSha256",
            "helperBuild",
            "files",
            "sourcePreservation",
            "databases",
            "safety",
        },
        context="manifest",
    )
    if manifest["schema"] != 1 or manifest["bundleId"] != "cortex-owner-product-proof-inputs-v1":
        raise api.ProofInputError("manifest identity is unsupported")
    if not api._is_readonly(manifest_path):
        raise api.ProofInputError("canonical bundle manifest is writable")
    if api.FULL_GIT_SHA.fullmatch(str(manifest["releaseGitSha"])) is None:
        raise api.ProofInputError("manifest release Git SHA is invalid")
    api._lower_sha256(manifest["contractSha256"], context="manifest contractSha256")
    api._lower_sha256(manifest["helperSha256"], context="manifest helperSha256")
    api._lower_sha256(manifest["helperSourceSha256"], context="manifest helperSourceSha256")
    helper_build = manifest["helperBuild"]
    if not isinstance(helper_build, dict):
        raise api.ProofInputError("manifest helper-build evidence must be an object")
    api._expect_keys(
        helper_build,
        {
            "mode",
            "releaseGitSha",
            "sourceTreeSha",
            "cargoLocked",
            "cargoOffline",
            "isolatedTarget",
            "reproducibleBuildProtocol",
            "sourceDateEpoch",
            "toolchainChannel",
            *api.HELPER_BUILD_TOOLCHAIN_FIELDS,
        },
        context="manifest helper-build evidence",
    )
    if helper_build["releaseGitSha"] != manifest["releaseGitSha"]:
        raise api.ProofInputError("helper build is not bound to the manifest release")
    if api.FULL_GIT_SHA.fullmatch(str(helper_build["sourceTreeSha"])) is None:
        raise api.ProofInputError("helper build source-tree identity is invalid")
    file_entries = api._manifest_files(manifest)
    expected_roles = set(api.SOURCE_ROLES) | {
        "proof-input-contract",
        "database-migration-helper",
        "database-migration-helper-source",
        "scale-database-derived-current",
    }
    if set(file_entries) != expected_roles:
        raise api.ProofInputError("manifest file-role inventory is not exact")
    api._assert_bundle_inventory(bundle, {item["relativePath"] for item in file_entries.values()})
    for role, item in file_entries.items():
        path = api._bundle_path(bundle, item["relativePath"])
        if require_namespace_seals and not api._access_is_denied(path, 0x00010000, directory=False):
            raise api.ProofInputError(f"published bundle file {role} lacks its deletion seal")
        api._assert_safe_existing_file(path, role=role, reject_protected=False, reject_snapshot=False)
        observed_hash, observed_size, _mode = api._hash_stable_file(path)
        if observed_hash != item["sha256"] or observed_size != item["sizeBytes"]:
            raise api.ProofInputError(f"bundle file {role} does not match its manifest")
        if not api._is_readonly(path):
            raise api.ProofInputError(f"bundle baseline file {role} is writable")
    contract_path = api._bundle_path(bundle, file_entries["proof-input-contract"]["relativePath"])
    contract = api.validate_contract(api._load_json(contract_path, canonical=True))
    if api._canonical_sha256(contract) != expected_contract_sha256:
        raise api.ProofInputError("bundled contract is not the exact release authority")
    if api._canonical_sha256(contract) != manifest["contractSha256"]:
        raise api.ProofInputError("bundled contract hash differs from the manifest")
    release_git = api._pinned_git_tool(contract["helperToolchain"]) if release_contract else None
    if release_contract:
        if helper_build != {
            "mode": "clean-isolated-cargo-locked-offline",
            "releaseGitSha": manifest["releaseGitSha"],
            "sourceTreeSha": api._git_tree_for_commit(manifest["releaseGitSha"], release_git),
            "cargoLocked": True,
            "cargoOffline": True,
            "isolatedTarget": True,
            "reproducibleBuildProtocol": api.REPRODUCIBLE_BUILD_PROTOCOL,
            "sourceDateEpoch": api._git_commit_timestamp(manifest["releaseGitSha"], release_git),
            **api._helper_toolchain_evidence(contract["helperToolchain"]),
        }:
            raise api.ProofInputError("release helper lacks exact clean isolated build evidence")
    elif helper_build["mode"] != "synthetic-test-override":
        raise api.ProofInputError("synthetic proof fixture has an unsupported helper-build identity")
    if manifest["contractSha256"] != file_entries["proof-input-contract"]["sha256"]:
        raise api.ProofInputError("contract file and canonical contract hash disagree")
    if manifest["helperSha256"] != file_entries["database-migration-helper"]["sha256"]:
        raise api.ProofInputError("helper file and manifest helper hash disagree")
    if manifest["helperSourceSha256"] != file_entries["database-migration-helper-source"]["sha256"]:
        raise api.ProofInputError("helper source file and manifest source hash disagree")
    specs = api._file_specs(contract)
    for role in api.SOURCE_ROLES:
        entry, spec = file_entries[role], specs[role]
        if entry["relativePath"] != spec["relativePath"] or entry["sha256"] != spec["sha256"]:
            raise api.ProofInputError(f"manifest {role} differs from the immutable contract")
        if "sizeBytes" in spec and entry["sizeBytes"] != spec["sizeBytes"]:
            raise api.ProofInputError(f"manifest {role} size differs from the immutable contract")
    preservation = manifest["sourcePreservation"]
    if not isinstance(preservation, list) or len(preservation) != len(api.SOURCE_ROLES):
        raise api.ProofInputError("source-preservation evidence inventory is incomplete")
    preservation_by_role: dict[str, dict[str, Any]] = {}
    for item in preservation:
        if not isinstance(item, dict):
            raise api.ProofInputError("source-preservation evidence must be an object")
        api._expect_keys(
            item,
            {"role", "declaredSha256", "copiedSha256", "verifiedStableBeforeAndAfter"},
            context="source-preservation evidence",
        )
        role = item["role"]
        if role not in api.SOURCE_ROLES or role in preservation_by_role:
            raise api.ProofInputError("source-preservation roles are missing, duplicated, or unknown")
        if (
            item["declaredSha256"] != specs[role]["sha256"]
            or item["copiedSha256"] != file_entries[role]["sha256"]
            or item["verifiedStableBeforeAndAfter"] is not True
        ):
            raise api.ProofInputError(f"source-preservation evidence for {role} is inconsistent")
        preservation_by_role[role] = item
    if set(preservation_by_role) != set(api.SOURCE_ROLES):
        raise api.ProofInputError("source-preservation evidence inventory is not exact")
    helper_path = api._bundle_path(bundle, file_entries["database-migration-helper"]["relativePath"])
    helper_source_path = api._bundle_path(bundle, file_entries["database-migration-helper-source"]["relativePath"])
    helper_source_hash, _helper_source_size, _helper_source_mode = api._hash_stable_file(helper_source_path)
    if helper_source_hash != manifest["helperSourceSha256"]:
        raise api.ProofInputError("bundled helper source changed")
    if release_contract and helper_source_hash != api._git_blob_sha256(
        manifest["releaseGitSha"], api.HELPER_REPO_PATH, release_git
    ):
        raise api.ProofInputError("bundled helper source is not the exact release commit blob")
    api._require_binary_git_marker(helper_path, manifest["releaseGitSha"])
    if release_contract and not allow_staging:
        api._verify_release_helper_rebuild(
            bundle,
            contract,
            manifest,
            file_entries["database-migration-helper"],
            release_git,
        )
    helper = helper_factory(
        helper_path,
        manifest["helperSha256"],
        manifest["releaseGitSha"],
        manifest["helperSourceSha256"],
    )
    scale_contract = contract["databaseContracts"]["scale"]
    campaign_contract = contract["databaseContracts"]["campaignExact"]
    api._require_helper_schema_contract(
        helper,
        schema=scale_contract["sourceSchemaVersion"],
        fingerprint=scale_contract["sourceSchemaFingerprintSha256"],
    )
    api._require_helper_schema_contract(
        helper,
        schema=scale_contract["targetSchemaVersion"],
        fingerprint=scale_contract["targetSchemaFingerprintSha256"],
    )
    api._require_helper_schema_contract(
        helper,
        schema=campaign_contract["schemaVersion"],
        fingerprint=campaign_contract["schemaFingerprintSha256"],
    )
    databases = manifest["databases"]
    if not isinstance(databases, dict):
        raise api.ProofInputError("manifest database evidence must be an object")
    api._expect_keys(
        databases,
        {"scaleAuthority", "scaleDerived", "campaignExactAuthority"},
        context="manifest databases",
    )
    checks = (
        (
            "scaleAuthority",
            file_entries["scale-database-authority"],
            scale_contract["sourceSchemaVersion"],
            scale_contract["sourceSchemaFingerprintSha256"],
            scale_contract["segmentCount"],
            scale_contract["distinctAudioPathCount"],
            "absent",
        ),
        (
            "campaignExactAuthority",
            file_entries["campaign-database-authority"],
            campaign_contract["schemaVersion"],
            campaign_contract["schemaFingerprintSha256"],
            campaign_contract["segmentCount"],
            campaign_contract["distinctAudioPathCount"],
            "required",
        ),
    )
    for evidence_name, file_entry, schema, fingerprint, segments, distinct, campaign in checks:
        path = api._bundle_path(bundle, file_entry["relativePath"])
        python_inspection = api.inspect_sqlite_readonly(path)
        helper_result = helper.inspect(path, expected_schema=schema, campaign=campaign)
        expected = api._compare_helper_inspection(
            helper_result,
            python_inspection,
            expected_hash=file_entry["sha256"],
            expected_schema=schema,
            expected_schema_fingerprint=fingerprint,
            expected_segments=segments,
            expected_distinct_paths=distinct,
            campaign=campaign,
        )
        if databases[evidence_name] != expected:
            raise api.ProofInputError(f"manifest {evidence_name} evidence is stale")
    derived_entry = file_entries["scale-database-derived-current"]
    derived_path = api._bundle_path(bundle, derived_entry["relativePath"])
    derived_python = api.inspect_sqlite_readonly(derived_path)
    derived_helper = helper.inspect(
        derived_path,
        expected_schema=scale_contract["targetSchemaVersion"],
        campaign="absent",
    )
    derived_expected = api._compare_helper_inspection(
        derived_helper,
        derived_python,
        expected_hash=derived_entry["sha256"],
        expected_schema=scale_contract["targetSchemaVersion"],
        expected_schema_fingerprint=scale_contract["targetSchemaFingerprintSha256"],
        expected_segments=scale_contract["segmentCount"],
        expected_distinct_paths=scale_contract["distinctAudioPathCount"],
        campaign="absent",
    )
    expected_derived_evidence = {
        **derived_expected,
        "authoritySha256": specs["scale-database-authority"]["sha256"],
        "appliedMigrations": list(
            range(scale_contract["sourceSchemaVersion"] + 1, scale_contract["targetSchemaVersion"] + 1)
        ),
    }
    if manifest["databases"]["scaleDerived"] != expected_derived_evidence:
        raise api.ProofInputError("manifest migrated scale evidence is stale")
    media_files = [path for path in (bundle / "media").rglob("*") if path.is_file()]
    if len(media_files) != contract["mediaFileCount"]:
        raise api.ProofInputError("media proof directory contains the wrong number of files")
    if sorted(path.suffix.casefold() for path in media_files) != contract["requiredMediaExtensions"]:
        raise api.ProofInputError("media proof directory contains the wrong formats")
    expected_safety = {
        "sourcePathsPersisted": False,
        "liveAppDataAccepted": False,
        "snapshotAcceptedAsWritableAttempt": False,
        "campaignPolicyDeletedOrRewritten": False,
        "attemptDeletionPolicy": "manual-only-never-deleted-by-this-tool",
    }
    if manifest["safety"] != expected_safety:
        raise api.ProofInputError("manifest safety contract is weakened")
    api._assert_manifest_has_no_private_paths(manifest)
    if release_contract and api._git_sha_clean(release_git) != manifest["releaseGitSha"]:
        raise api.ProofInputError("release checkout changed during proof-bundle validation")
    return manifest


def canonical_run_token(api: Any, value: str) -> str:
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError) as error:
        raise api.ProofInputError("run token must be a canonical UUIDv4") from error
    if parsed.version != 4 or str(parsed) != value:
        raise api.ProofInputError("run token must be a lowercase canonical UUIDv4")
    return value


def attempt_result(
    api: Any,
    bundle: Path,
    final: Path,
    token: str,
    files: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    return {
        "schema": 1,
        "runToken": token,
        "attemptDirectory": os.fspath(final),
        "environment": {
            "CORTEX_OWNER_REAL_MEDIA_DIR": os.fspath(bundle / "media"),
            "CORTEX_OWNER_AUDIOBOOK_MP3": os.fspath(
                api._bundle_path(bundle, str(files["long-audiobook-mp3"]["relativePath"]))
            ),
            "CORTEX_OWNER_SCALE_DB": os.fspath(final / "scale-work.db"),
        },
        "campaignObservationDb": os.fspath(final / "campaign-observation.db"),
    }


def build_attempt_manifest(
    api: Any,
    token: str,
    manifest: Mapping[str, Any],
    files: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    """Construct the one exact initial-state authority accepted for attempt replay."""
    return {
        "schema": 1,
        "runToken": token,
        "releaseGitSha": manifest["releaseGitSha"],
        "bundleManifestSha256": hashlib.sha256(api.canonical_json_bytes(manifest)).hexdigest(),
        "files": [
            {
                "role": "scale-writable-attempt",
                "relativePath": "scale-work.db",
                "initialSha256": files["scale-database-derived-current"]["sha256"],
                "initialSizeBytes": files["scale-database-derived-current"]["sizeBytes"],
                "campaignAuthority": "absent",
            },
            {
                "role": "campaign-characterization-attempt",
                "relativePath": "campaign-observation.db",
                "initialSha256": files["campaign-database-authority"]["sha256"],
                "initialSizeBytes": files["campaign-database-authority"]["sizeBytes"],
                "campaignAuthority": "required-and-never-sanitized",
            },
        ],
        "cleanupPolicy": "manual-only-never-deleted-by-this-tool",
    }


def recover_attempt(
    api: Any,
    bundle: Path,
    final: Path,
    token: str,
    manifest: Mapping[str, Any],
    files: Mapping[str, Mapping[str, Any]],
) -> dict[str, Any]:
    attempt = api._assert_no_links(final)
    if not attempt.is_dir():
        raise api.ProofInputError("preexisting run token is not one direct attempt directory")
    api._require_directory_denials(attempt, (0x00010000, 0x4, 0x40), context="preexisting attempt")
    if api._access_is_denied(attempt, 0x2, directory=True):
        raise api.ProofInputError("preexisting attempt cannot create SQLite durability sidecars")
    attempt_manifest_path = attempt / "attempt-manifest.v1.json"
    if not api._access_is_denied(attempt_manifest_path, 0x00010000, directory=False):
        raise api.ProofInputError("preexisting attempt manifest lacks its deletion seal")
    if not api._is_readonly(attempt_manifest_path):
        raise api.ProofInputError("preexisting attempt manifest is writable")
    attempt_manifest = api._load_json(attempt_manifest_path, canonical=True)
    if attempt_manifest != build_attempt_manifest(api, token, manifest, files):
        raise api.ProofInputError("preexisting run token manifest is not the exact initial authority")
    expected = {
        "scale-work.db": str(files["scale-database-derived-current"]["sha256"]),
        "campaign-observation.db": str(files["campaign-database-authority"]["sha256"]),
    }
    declared = attempt_manifest["files"]
    if not isinstance(declared, list) or {
        item.get("relativePath") for item in declared if isinstance(item, dict)
    } != set(expected):
        raise api.ProofInputError("preexisting attempt file inventory is not exact")
    observed = {
        path.name
        for path in attempt.iterdir()
        if path.name != "attempt-manifest.v1.json" and path.name not in api.PUBLISHED_TRANSACTION_FILES
    }
    if observed != set(expected):
        raise api.ProofInputError("preexisting attempt contains an undeclared entry")
    for name, expected_hash in expected.items():
        path = api._assert_safe_existing_file(
            attempt / name,
            role="preexisting writable attempt",
            reject_protected=False,
            reject_snapshot=False,
        )
        if not api._access_is_denied(path, 0x00010000, directory=False):
            raise api.ProofInputError("preexisting attempt file lacks its deletion seal")
        if api._is_readonly(path):
            raise api.ProofInputError("preexisting attempt database is read-only")
        before = api._state(path)
        descriptor = -1
        try:
            descriptor = os.open(
                path,
                os.O_RDWR | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0),
            )
            opened = os.fstat(descriptor)
        except OSError as error:
            raise api.ProofInputError("preexisting attempt database cannot be opened writable") from error
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        after = api._state(path)
        if before != after or (opened.st_dev, opened.st_ino, opened.st_size) != before[:3]:
            raise api.ProofInputError("preexisting attempt database changed while proving write access")
        if api._hash_stable_file(path)[0] != expected_hash:
            raise api.ProofInputError("preexisting attempt changed after publication and cannot be replayed")
    return api._attempt_result(bundle, final, token, files)
