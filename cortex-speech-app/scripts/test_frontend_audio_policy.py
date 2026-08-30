import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


def test_audio_player_playback_failures_are_visible() -> None:
    audio_player = (REPO_ROOT / "src/lib/AudioPlayer.svelte").read_text(encoding="utf-8")
    controller = (REPO_ROOT / "src/lib/audioPlayerController.ts").read_text(encoding="utf-8")
    surface = f"{audio_player}\n{controller}"
    forbidden = [
        "audioEl.play().catch(() => {});",
        "notifications.error(message, { detail: String(cause) });",
        "notifications.error(message, { detail: formatUnknownError(cause) });",
    ]
    present = [pattern for pattern in forbidden if pattern in surface]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"AudioPlayer.svelte silently swallows playback failures:\n{formatted}")

    required = [
        "import { notifications } from './stores/notificationStore';",
        "private reportPlaybackFailure(",
        # `audioError` is the $bindable prop the decision guards read (2026-08-17): a failure is not
        # just shown, it also blocks Accept/Reject on audio nobody could hear. The pin follows the
        # rename — the requirement is unchanged, every failure still lands in visible state.
        "this.output.setAudioError(message);",
        "notifications.error(message, { cause });",
        "this.attemptPlay(this.output.translate('audio.playbackFailed'));",
        "this.attemptPlay(this.output.translate('audio.loopFailed'));",
        "this.output.setAudioError(this.output.translate('audio.loadFailed'));",
    ]
    missing = [pattern for pattern in required if pattern not in surface]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"AudioPlayer.svelte must keep playback failures visible:\n{formatted}")

    # A translation call is only as safe as its catalogs. Require every visible audio failure key in
    # both supported locales, with a non-empty human-facing value (never a raw-key fallback).
    for catalog_name in ("en", "ckb"):
        catalog = (REPO_ROOT / f"src/lib/i18n/{catalog_name}.ts").read_text(encoding="utf-8")
        for key in ("audio.loadFailed", "audio.playbackFailed", "audio.loopFailed"):
            match = re.search(rf"'{re.escape(key)}'\s*:\s*'([^']+)'", catalog)
            if not match or not match.group(1).strip() or match.group(1).strip() == key:
                raise AssertionError(
                    f"{catalog_name}.ts must define a non-empty localized value for {key!r}"
                )


def test_audio_player_loop_failure_has_unit_coverage() -> None:
    test_file = (REPO_ROOT / "tests/lib/AudioPlayer.test.ts").read_text(encoding="utf-8")
    required = [
        "reports loop replay failures instead of silently swallowing them",
        "playMock.mockRejectedValueOnce(new Error('autoplay denied'));",
        "locale.set('ckb');",
        "expect(screen.getByText(ckb['audio.loopFailed'])).toBeInTheDocument();",
        "item.type === 'error' && item.message === ckb['audio.loopFailed']",
    ]
    missing = [pattern for pattern in required if pattern not in test_file]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"AudioPlayer playback failure coverage is incomplete:\n{formatted}")


def test_audio_state_machine_is_clip_attempt_bound_and_stress_tested() -> None:
    audio_player = (REPO_ROOT / "src/lib/AudioPlayer.svelte").read_text(encoding="utf-8")
    controller = (REPO_ROOT / "src/lib/audioPlayerController.ts").read_text(encoding="utf-8")
    machine = (REPO_ROOT / "src/lib/audioMachine.ts").read_text(encoding="utf-8")
    tests = (REPO_ROOT / "tests/lib/audioMachine.test.ts").read_text(encoding="utf-8")
    player_pins = [
        "isCurrentAudioAttempt(this.audioMachine, binding)",
        "this.transition({ type: 'select', clipId, sourceId })",
        "this.transition({ type: 'failed', binding, errorCode: 'AUDIO_DECODE_FAILED' })",
        "activePlayBinding",
        "mediaLoadBinding",
    ]
    machine_pins = [
        "export const AUDIO_PHASES",
        "'resolving'",
        "'loading'",
        "'ready'",
        "'playing'",
        "'paused'",
        "'ended'",
        "'failed'",
        "'blocked'",
        "export function isCurrentAudioAttempt",
        "a late resolver, play promise, media failure, ended event, or timer is a no-op",
    ]
    test_pins = [
        "survives 10,000 randomized transitions without cross-clip state corruption",
        "for (let step = 0; step < 10_000; step += 1)",
        "if (wasStale) expect(state).toBe(before);",
    ]
    missing = [
        *(f"AudioPlayer controller: {pin}" for pin in player_pins if pin not in controller),
        *(f"audioMachine: {pin}" for pin in machine_pins if pin not in machine),
        *(f"audioMachine test: {pin}" for pin in test_pins if pin not in tests),
    ]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"Audio attempt state-machine contract is incomplete:\n{formatted}")


def main() -> None:
    test_audio_player_playback_failures_are_visible()
    test_audio_player_loop_failure_has_unit_coverage()
    test_audio_state_machine_is_clip_attempt_bound_and_stress_tested()
    print("frontend audio policy regression passed")


if __name__ == "__main__":
    main()
