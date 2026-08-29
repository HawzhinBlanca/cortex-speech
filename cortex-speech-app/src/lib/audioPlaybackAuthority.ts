import { beginDesktopPlaybackSessionV1, cancelDesktopPlaybackSessionV1 } from './commands';
import type { AudioAttemptBinding } from './audioMachine';

interface PlaybackAuthorityPort {
  playbackReceiptId: () => string | null;
  setPlaybackReceiptId: (value: string | null) => void;
  setPlaybackMediaGrantId: (value: string | null) => void;
  setPlaybackClipDurationMs: (value: number | null) => void;
  setPending: (value: boolean) => void;
}

export class AudioPlaybackAuthority {
  private controller: AbortController | null = null;
  private clientAttemptId: string | null = null;
  private segmentId: string | null = null;
  private revision: number | null = null;

  constructor(private readonly port: PlaybackAuthorityPort) {}

  createAttempt(enabled: boolean): string | null {
    this.clientAttemptId = enabled ? crypto.randomUUID() : null;
    return this.clientAttemptId;
  }

  private retire(receiptId: string | null, clientAttemptId: string | null): void {
    if (!receiptId || !clientAttemptId) return;
    void cancelDesktopPlaybackSessionV1(receiptId, clientAttemptId).catch(() => undefined);
  }

  forget(): void {
    this.controller?.abort();
    this.controller = null;
    this.port.setPending(false);
    this.port.setPlaybackReceiptId(null);
    this.port.setPlaybackMediaGrantId(null);
    this.port.setPlaybackClipDurationMs(null);
    this.clientAttemptId = null;
    this.segmentId = null;
    this.revision = null;
  }

  clear(): void {
    const receiptId = this.port.playbackReceiptId();
    const attemptId = this.clientAttemptId;
    this.forget();
    this.retire(receiptId, attemptId);
  }

  retireIssued(receiptId: string | null, clientAttemptId: string | null): void {
    this.retire(receiptId, clientAttemptId);
  }

  forgetIfCurrent(receiptId: string | null, clientAttemptId: string | null): void {
    if (
      receiptId &&
      this.port.playbackReceiptId() === receiptId &&
      this.clientAttemptId === clientAttemptId
    ) {
      this.forget();
    }
  }

  async begin(
    mediaGrantId: string,
    binding: AudioAttemptBinding,
    revision: number | undefined,
    clientAttemptId: string,
    isCurrent: () => boolean,
    onFailure: (error: unknown) => void,
  ): Promise<boolean> {
    this.controller?.abort();
    const controller = new AbortController();
    this.controller = controller;
    this.port.setPending(true);
    this.port.setPlaybackReceiptId(null);
    this.port.setPlaybackMediaGrantId(null);
    this.port.setPlaybackClipDurationMs(null);
    let issuedReceiptId: string | null = null;
    try {
      if (!Number.isSafeInteger(revision) || (revision ?? -1) < 0) {
        throw new Error('playback proof requires the exact rendered review revision');
      }
      const session = await beginDesktopPlaybackSessionV1(
        binding.clipId,
        mediaGrantId,
        revision as number,
        clientAttemptId,
      );
      issuedReceiptId = session.playbackReceiptId || null;
      if (controller.signal.aborted || !isCurrent() || this.clientAttemptId !== clientAttemptId) {
        this.retire(issuedReceiptId, clientAttemptId);
        return false;
      }
      if (
        session.segmentId !== binding.clipId ||
        session.segmentRevision !== revision ||
        !session.playbackReceiptId ||
        !Number.isSafeInteger(session.clipDurationMs) ||
        session.clipDurationMs <= 0
      ) {
        throw new Error('playback session identity mismatch');
      }
      this.port.setPlaybackReceiptId(session.playbackReceiptId);
      this.port.setPlaybackMediaGrantId(mediaGrantId);
      this.port.setPlaybackClipDurationMs(session.clipDurationMs);
      this.segmentId = binding.clipId;
      this.revision = revision as number;
      issuedReceiptId = null;
      return true;
    } catch (error) {
      this.retire(issuedReceiptId, clientAttemptId);
      if (!controller.signal.aborted && isCurrent()) onFailure(error);
      return false;
    } finally {
      if (this.controller === controller) {
        this.controller = null;
        this.port.setPending(false);
      }
    }
  }

  snapshotIdentity(): { segmentId: string | null; segmentRevision: number | null } {
    return { segmentId: this.segmentId, segmentRevision: this.revision };
  }
}
