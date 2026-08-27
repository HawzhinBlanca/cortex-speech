import type { Page } from '@playwright/test';

/** Minimal Tauri internals mock for Vite-only Playwright runs (no desktop shell). */
export async function installTauriMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const mockSegment = {
      id: 'e2e-segment-1',
      audioPath: 'sample.wav',
      rawTranscript: 'hello world',
      normalizedTranscript: 'hello world',
      annotatedTranscript: 'hello world',
      alignmentJson: null as string | null,
      durationMs: 1500,
      speakerId: 'SPEAKER_00' as string | null,
      verified: false,
    };

    const mockSettings = {
      model_dir: '',
      output_dir: '',
      asr_provider: 'SherpaOnnxCtc',
      // Mirror the production factory contract: the fine-tuned OmniASR-7B champion is the sole
      // automatic ASR. Smaller/cloud engines must not appear merely because the browser fixture is
      // older than the backend default.
      asr_model_size: 'WSL7B',
      multi_engine_hypotheses: false,
      use_finetuned_asr: false,
      external_asr_script_path: '/root/cortex_env/cortex_7b_client.py',
      vad_threshold: 0.5,
      min_segment_duration_ms: 3000,
      max_segment_duration_ms: 15000,
      num_asr_threads: 4,
      enable_gpu: false,
      language: 'ckb',
      export_format: 'Json',
      auto_normalize: true,
      auto_align: false,
      assign_speaker_from_filename: true,
      enable_diarization: true,
      max_speakers: 8,
      max_wer_threshold: 0.35,
      max_cer_threshold: 0.2,
      enforce_quality_gates: false,
      theme: 'Dark',
    };

    const mockQuality = {
      totalSegments: 1,
      emptyTranscriptCount: 0,
      lowConfidenceCount: 0,
      duplicateTranscriptGroups: 0,
      duplicateTranscriptSegments: 0,
      durationOutlierCount: 0,
      medianDurationMs: 1500,
      q1DurationMs: 1500,
      q3DurationMs: 1500,
      duplicateGroups: [],
      durationOutliers: [],
      annotatedSegmentCount: 1,
      meanWer: 0.0,
      meanCer: 0.0,
      segmentsAboveWerThreshold: 0,
      segmentsAboveCerThreshold: 0,
      qualityGatePassed: true,
      werOutliers: [],
    };

    // Matches the real get_dataset_certificate contract (Result<ConformalCertificate, String>,
    // always Ok — sparse data yields a heuristic, not-calibrated cert). Without this the harness's
    // default null tripped `cert.threshold` and logged a misleading console.error every run.
    const mockCertificate = {
      targetError: 0.05,
      confidenceLevel: 0.95,
      threshold: 0.35,
      totalCertified: 0,
      certifiedSegmentIds: [],
      expectedErrorBound: 0.05,
      isCalibrated: false,
    };
    const emptyLibrary = () => window.localStorage.getItem('__cortex_e2e_empty_library__') === '1';

    let eventId = 1;
    const eventHandlers = new Map<number, (payload: unknown) => void>();
    const eventListenerIds = new Map<string, number[]>();

    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: (event: string, id: number) => {
        eventHandlers.delete(id);
        const ids = eventListenerIds.get(event);
        if (ids) {
          eventListenerIds.set(
            event,
            ids.filter((existing) => existing !== id),
          );
        }
      },
    };

    window.__TAURI_INTERNALS__ = {
      transformCallback: (callback: (payload: unknown) => void, _once = false) => {
        const id = eventId++;
        eventHandlers.set(id, callback);
        return id;
      },
      unregisterCallback: (id: number) => {
        eventHandlers.delete(id);
      },
      convertFileSrc: (path: string) => path,
      invoke: async (
        cmd: string,
        args?: {
          ids?: string[];
          speakerId?: string;
          searchQuery?: string;
          sortOrder?: string;
          id?: string;
          audioPath?: string;
          segmentId?: string;
          expectedRevision?: number;
          clientAttemptId?: string;
          playbackReceiptId?: string;
          intervals?: Array<{ startMs?: number; endMs?: number }>;
        },
      ) => {
        switch (cmd) {
          case 'get_segments_page':
            return emptyLibrary()
              ? { items: [], total: 0, nextCursor: null }
              : { items: [mockSegment], total: 1, nextCursor: null };
          case 'get_review_page_v1':
            return emptyLibrary()
              ? {
                  items: [],
                  total: 0,
                  nextCursor: null,
                  scopeLabel: 'pending',
                  focusNarrowed: false,
                }
              : {
                  items: [
                    {
                      segment: mockSegment,
                      baseRevision: 0,
                      eligible: true,
                      disabledReason: null,
                    },
                  ],
                  total: 1,
                  nextCursor: null,
                  scopeLabel: 'pending',
                  focusNarrowed: false,
                };
          case 'get_review_draft_v1':
            return null;
          case 'get_segment':
            if (emptyLibrary()) throw new Error('Segment no longer exists');
            return mockSegment;
          case 'get_segment_ids_for_view':
            return emptyLibrary() ? [] : [mockSegment.id];
          case 'get_signal_anomaly_segments':
            return [];
          case 'app_health':
            // Healthy report matching the real app_health contract, so the health loop's
            // real code path runs in e2e instead of dereferencing the default null.
            return {
              db_ok: true,
              db_size_bytes: 1024,
              memory_mb: 100,
              missing_models: [],
              missing_optional_models: [],
              snapshot_last_success_epoch_secs: Math.floor(Date.now() / 1000),
              snapshot_consecutive_failures: 0,
              free_disk_bytes: 100 * 1024 ** 3,
            };
          case 'get_segments':
            return [mockSegment];
          case 'get_settings':
            return mockSettings;
          case 'get_history_status_v1':
            return { undoAction: null, redoAction: null };
          case 'undo':
          case 'redo':
            return { action: null, status: { undoAction: null, redoAction: null } };
          case 'get_dataset_quality':
            return mockQuality;
          case 'get_dataset_certificate':
            return mockCertificate;
          case 'register_media_asset':
          case 'register_review_media_asset':
            return {
              id: 'e2e-audio-grant',
              path: String(args?.audioPath ?? ''),
              expiresAt: new Date(Date.now() + 60_000).toISOString(),
            };
          case 'get_media_asset_url':
            // Valid empty WAV. Keeping playback on a data URL exercises the successful grant path
            // without leaking test requests to the Vite server or flooding logs with expected 404s.
            return 'data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEARKwAAIhYAQACABAAZGF0YQAAAAA=';
          case 'begin_desktop_playback_session_v1':
            return {
              playbackReceiptId: crypto.randomUUID(),
              segmentId: String(args?.segmentId ?? mockSegment.id),
              segmentRevision: Number(args?.expectedRevision ?? 0),
              clipDurationMs: mockSegment.durationMs,
              expiresAtMs: Date.now() + 30 * 60_000,
            };
          case 'finalize_desktop_playback_session_v1': {
            const uniquePlayedMs = (args?.intervals ?? []).reduce(
              (total, interval) =>
                total + Math.max(0, Number(interval.endMs ?? 0) - Number(interval.startMs ?? 0)),
              0,
            );
            return {
              playbackReceiptId: String(args?.playbackReceiptId ?? ''),
              segmentId: String(args?.segmentId ?? mockSegment.id),
              segmentRevision: Number(args?.expectedRevision ?? 0),
              uniquePlayedMs,
              clipDurationMs: mockSegment.durationMs,
              coverageRatio: Math.min(1, uniquePlayedMs / mockSegment.durationMs),
            };
          }
          case 'cancel_desktop_playback_session_v1':
            return true;
          case 'get_waveform':
            return [0.1, 0.35, 0.8, 0.4, 0.15];
          case 'get_audio_duration':
            return 1.5;
          case 'get_audio_health':
            return { totalFiles: 1, missingFiles: 0, missingPaths: [] };
          case 'take_last_crash':
            return null;
          case 'get_training_grade_breakdown':
            // Match the fail-closed readiness contract. Falling through to null/object stubs makes
            // the Insights panel log an error on every E2E page load and leaves the accessibility
            // scan racing a permanently degraded render.
            return {
              summary: {
                totalSegments: 1,
                trainingReadySegments: 0,
                goldSegments: 0,
                silverSegments: 0,
                reviewSegments: 1,
                rejectedSegments: 0,
              },
              reasonCounts: { not_human_or_high_confidence_agent_verified: 1 },
            };
          case 'get_configured_providers':
            // Names only, never key values — matches the real configured_providers() contract.
            return ['gemini'];
          case 'set_api_key':
            // Echo the post-save provider-NAMES list (never a key value), like the real command.
            return ['gemini', args?.provider ?? 'openrouter'];
          case 'update_segment_metadata_v1': {
            const request = args?.request as
              | {
                  segmentId?: string;
                  changes?: Array<{
                    field?: 'speakerId' | 'alignmentJson';
                    expected?: string | null;
                    value?: string | null;
                  }>;
                }
              | undefined;
            const speaker = request?.changes?.find((change) => change.field === 'speakerId');
            const alignment = request?.changes?.find((change) => change.field === 'alignmentJson');
            if (
              (speaker &&
                mockSegment.speakerId !== speaker.expected &&
                mockSegment.speakerId !== speaker.value) ||
              (alignment &&
                mockSegment.alignmentJson !== alignment.expected &&
                mockSegment.alignmentJson !== alignment.value)
            ) {
              throw {
                schema: 1,
                code: 'STALE_SEGMENT_METADATA',
                message: 'The mock metadata changed. Reload it before saving.',
                retryable: false,
                suggestedAction: 'reloadClip',
              };
            }
            if (speaker) mockSegment.speakerId = speaker.value ?? null;
            if (alignment) mockSegment.alignmentJson = alignment.value ?? null;
            return {
              segmentId: request?.segmentId ?? 'seg-1',
              speakerId: mockSegment.speakerId,
              alignmentJson: mockSegment.alignmentJson,
              changed: true,
            };
          }
          case 'couch_review_status':
            return { running: false, reviewers: [] };
          case 'start_couch_review':
            // v43 multi-reviewer: one entry PER named reviewer, each with its own token. Mirror the real
            // command by echoing the requested names, so a mock session cannot pass with a shape the
            // backend no longer returns.
            return {
              running: true,
              reviewers: ((args?.reviewers as string[] | undefined)?.length
                ? (args?.reviewers as string[])
                : ['owner']
              ).map((name, i) => ({
                name,
                url: `http://192.168.0.2:8737/?t=mock-token-${i}`,
                tailscaleUrl: `http://100.64.0.2:8737/?t=mock-token-${i}`,
              })),
            };
          case 'stop_couch_review':
            return { running: false, reviewers: [] };
          case 'reviewer_throughput':
            return []; // an ARRAY, never null - see the spot_check_report note below
          case 'revoke_couch_reviewer':
            return { running: true, reviewers: [] };
          case 'spot_check_report':
            // An ARRAY, never null. Returning null here is what took the settings dialog down: the
            // panel rendered `spotChecks.length` on it and threw mid-render.
            return [];
          case 'export_agreement_sample':
            return null; // nothing double-reviewed in a mock session — the null path the UI must handle
          case 'get_fingerprint_count':
            return 1;
          case 'get_tracing_stats':
            return { total_spans: 2, failures: 0, total_duration_ms: 12.5, avg_duration_ms: 6.25 };
          case 'get_recent_spans':
            return [
              {
                operation: 'diff.compute',
                start: '0',
                duration_ms: 5.0,
                metadata: {},
                success: true,
                error: null,
              },
              {
                operation: 'asr.transcribe',
                start: '0',
                duration_ms: 7.5,
                metadata: {},
                success: true,
                error: null,
              },
            ];
          case 'clear_tracing_spans':
            return null;
          case 'import_model_checkpoint':
            return args?.id ?? 'imported-candidate';
          case 'plugin:dialog|open':
            // Simulate the native file picker returning a chosen path.
            return '/fake/path/to/checkpoint.onnx';
          case 'list_eval_runs':
            return [];
          case 'get_escalation_rate_trend':
            return [];
          case 'get_label_quality_lift':
            return null;
          case 'run_gold_eval_asr':
          case 'run_gold_eval_local':
            return {
              run: {
                id: 'eval-run-1',
                modelId: 'omniasr-ctc-300m',
                runAt: '2026-06-25T00:00:00Z',
                numSegs: 40,
                wer: 0.6,
                cer: 0.29,
              },
              segments: [],
            };
          case 'build_scorecard':
            return { scorecard: {}, markdown: '# Scorecard\n\nmicro CER: 29.0%\n' };
          case 'create_gold_from_file':
            return 5;
          case 'save_session': {
            // Persist view-state in localStorage so a reload restores it (the real backend persists
            // to session.json). Per-context, so it never leaks across tests.
            try {
              window.localStorage.setItem(
                '__cortex_session__',
                JSON.stringify({
                  search_query: args?.searchQuery ?? '',
                  sort_order: args?.sortOrder ?? 'newest',
                }),
              );
            } catch {
              /* ignore storage failures in tests */
            }
            return null;
          }
          case 'restore_session': {
            try {
              const raw = window.localStorage.getItem('__cortex_session__');
              if (!raw) return null;
              const parsed = JSON.parse(raw) as { search_query?: string; sort_order?: string };
              return {
                search_query: parsed.search_query ?? '',
                sort_order: parsed.sort_order ?? 'newest',
                segment_count: 1,
                verified_count: 0,
              };
            } catch {
              return null;
            }
          }
          case 'list_model_versions':
            return [
              {
                id: 'omniasr-7b-champion',
                family: 'omniasr-7b',
                model_card_name: 'Pinned Kurdish champion deployment',
                checkpoint_sha256:
                  'a1b2c3d4e5f600112233445566778899aabbccddeeff00112233445566778899',
                source: 'owner-finetune',
                license: 'Apache-2.0',
                status: 'champion',
              },
              {
                id: 'omniasr-7b-challenger',
                family: 'omniasr-7b',
                model_card_name: null,
                checkpoint_sha256:
                  '00112233445566778899aabbccddeeffa1b2c3d4e5f6000000000000deadbeef',
                source: 'owner-finetune',
                license: 'Apache-2.0',
                status: 'candidate',
              },
            ];
          case 'models_status':
            return [
              {
                name: 'Silero VAD v4',
                filename: 'silero_vad_v4.onnx',
                downloaded: true,
                exists: true,
                size_bytes: 2_000_000,
                min_size_bytes: 1_000_000,
                version: '4.0',
                source: 'bundled',
                downloadable: true,
              },
              {
                name: 'CAM++ Speaker Embedding',
                filename: 'campp/model.onnx',
                downloaded: true,
                exists: true,
                size_bytes: 12_000_000,
                min_size_bytes: 10_000_000,
                version: '1.0',
                source: 'bundled',
                downloadable: true,
              },
              {
                name: 'AI Audio Denoiser',
                filename: 'denoiser/model.onnx',
                downloaded: true,
                exists: true,
                size_bytes: 500_000,
                min_size_bytes: 400_000,
                version: '1.0',
                source: 'bundled',
                downloadable: true,
              },
            ];
          case 'models_download_all':
            return { downloaded: 0, failed: 0, total: 0, skipped: 0 };
          case 'get_inference_stats':
            return {
              vad: { calls: 0, failures: 0, p50_ms: 0, p99_ms: 0 },
              asr: { calls: 0, failures: 0, p50_ms: 0, p99_ms: 0 },
              model_load_ms: 0,
            };
          case 'get_dataset_stats':
            if (emptyLibrary()) {
              return {
                totalSegments: 0,
                verifiedCount: 0,
                pendingCount: 0,
                totalDurationSeconds: 0,
                verificationRate: 0,
                uniqueSpeakers: 0,
                durationHistogram: {
                  under5s: 0,
                  under10s: 0,
                  under15s: 0,
                  under30s: 0,
                  over30s: 0,
                },
                topSpeakers: [],
              };
            }
            return {
              totalSegments: 1,
              verifiedCount: 0,
              pendingCount: 1,
              totalDurationSeconds: 1.5,
              verificationRate: 0,
              uniqueSpeakers: 1,
              durationHistogram: {
                under5s: 1,
                under10s: 0,
                under15s: 0,
                under30s: 0,
                over30s: 0,
              },
              topSpeakers: [
                { speakerId: 'SPEAKER_00', segmentCount: 1, totalDurationSeconds: 1.5 },
              ],
            };
          case 'validate_dataset_cmd':
            return {
              totalSegments: 1,
              totalAudioFiles: 1,
              passed: 1,
              warnings: [],
              errors: [],
              summary: '1 segment checked — no issues',
            };
          case 'delete_segments_v1': {
            const ids = (args?.request as { ids?: string[] } | undefined)?.ids ?? [];
            return { requestedCount: ids.length, deletedCount: ids.length };
          }
          case 'get_speaker_inventory_v1':
            return [{ speakerId: 'SPEAKER_00', segmentCount: 1, totalDurationSeconds: 1.5 }];
          case 'rename_speaker_v1': {
            const request = args?.request as
              | {
                  sourceSpeakerId?: string | null;
                  targetSpeakerId?: string;
                  expectedSourceCount?: number;
                  expectedTargetCount?: number;
                }
              | undefined;
            return {
              sourceSpeakerId: request?.sourceSpeakerId ?? null,
              targetSpeakerId: request?.targetSpeakerId ?? '',
              renamedCount: request?.expectedSourceCount ?? 0,
              targetCount:
                (request?.expectedSourceCount ?? 0) + (request?.expectedTargetCount ?? 0),
              merged: (request?.expectedTargetCount ?? 0) > 0,
            };
          }
          case 'assign_speakers_v1': {
            const request = args?.request as
              { ids?: string[]; targetSpeakerId?: string | null } | undefined;
            const ids = request?.ids ?? [];
            return { requestedCount: ids.length, changedCount: ids.length, unchangedCount: 0 };
          }
          case 'export_huggingface_dataset':
            return null;
          case 'batch_verify':
          case 'batch_normalize':
          case 'batch_transcribe':
          case 'rediarize_segments':
            return { status: 'started' };
          case 'plugin:event|listen': {
            const eventName = (args as { event?: string; handler?: number } | undefined)?.event;
            const handlerId = (args as { event?: string; handler?: number } | undefined)?.handler;
            if (eventName && typeof handlerId === 'number') {
              const ids = eventListenerIds.get(eventName) ?? [];
              ids.push(handlerId);
              eventListenerIds.set(eventName, ids);
            }
            return handlerId ?? 1;
          }
          case 'plugin:event|unlisten':
            return null;
          default:
            throw new Error(`Unknown E2E Tauri mock command: ${cmd}`);
        }
      },
      // @tauri-apps/api/window reads these labels before registering the close-request handler.
      // Without them Vite E2E emits a TypeError on every mount, hiding real console regressions.
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
    };

    (
      window as unknown as { __emitTauriEvent?: (event: string, payload: unknown) => void }
    ).__emitTauriEvent = (event: string, payload: unknown) => {
      const ids = eventListenerIds.get(event) ?? [];
      for (const id of ids) {
        const cb = eventHandlers.get(id);
        cb?.({ event, id, payload });
      }
    };
  });
}

/** Emit a mocked Tauri event into the page (for progress UI tests). */
export async function emitTauriEvent(page: Page, event: string, payload: unknown): Promise<void> {
  await page.evaluate(
    ({ event, payload }) => {
      const w = window as unknown as { __emitTauriEvent?: (e: string, p: unknown) => void };
      w.__emitTauriEvent?.(event, payload);
    },
    { event, payload },
  );
}
