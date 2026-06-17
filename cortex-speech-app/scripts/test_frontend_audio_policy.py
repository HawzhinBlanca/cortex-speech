from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


def test_audio_player_playback_failures_are_visible() -> None:
    audio_player = (REPO_ROOT / "src/lib/AudioPlayer.svelte").read_text(encoding="utf-8")
    forbidden = [
        "audioEl.play().catch(() => {});",
    ]
    present = [pattern for pattern in forbidden if pattern in audio_player]
    if present:
        formatted = "\n".join(f"- {entry}" for entry in present)
        raise AssertionError(f"AudioPlayer.svelte silently swallows playback failures:\n{formatted}")

    required = [
        "import { notifications } from './stores/notificationStore';",
        "function reportPlaybackFailure(message: string, cause: unknown)",
        "notifications.error(message, { detail: String(cause) });",
        "attemptPlay('Playback blocked or file not found');",
        "attemptPlay('Loop playback failed');",
    ]
    missing = [pattern for pattern in required if pattern not in audio_player]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"AudioPlayer.svelte must keep playback failures visible:\n{formatted}")


def test_audio_player_loop_failure_has_unit_coverage() -> None:
    test_file = (REPO_ROOT / "tests/lib/AudioPlayer.test.ts").read_text(encoding="utf-8")
    required = [
        "reports loop replay failures instead of silently swallowing them",
        "playMock.mockRejectedValueOnce(new Error('autoplay denied'));",
        "expect(screen.getByText('Loop playback failed')).toBeInTheDocument();",
        "item.type === 'error' && item.message === 'Loop playback failed'",
    ]
    missing = [pattern for pattern in required if pattern not in test_file]
    if missing:
        formatted = "\n".join(f"- {entry}" for entry in missing)
        raise AssertionError(f"AudioPlayer playback failure coverage is incomplete:\n{formatted}")


def main() -> None:
    test_audio_player_playback_failures_are_visible()
    test_audio_player_loop_failure_has_unit_coverage()
    print("frontend audio policy regression passed")


if __name__ == "__main__":
    main()
