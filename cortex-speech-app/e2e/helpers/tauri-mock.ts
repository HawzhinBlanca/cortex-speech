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
      alignmentJson: null,
      durationMs: 1500,
      speakerId: 'SPEAKER_00',
      verified: false,
    };

    const mockSettings = {
      model_dir: '',
      output_dir: '',
      asr_provider: 'Local',
      asr_model_size: 'CTC300M',
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
      max_cer_threshold: 0.20,
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
        },
      ) => {
        switch (cmd) {
          case 'get_segments_page':
            return { items: [mockSegment], total: 1, nextCursor: null };
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
          case 'get_dataset_quality':
            return mockQuality;
          case 'get_dataset_certificate':
            return mockCertificate;
          case 'get_configured_providers':
            // Names only, never key values — matches the real configured_providers() contract.
            return ['elevenlabs'];
          case 'set_api_key':
            // Echo the post-save provider-NAMES list (never a key value), like the real command.
            return ['elevenlabs', args?.provider ?? 'openrouter'];
          case 'update_segment_fields':
            // F10 partial autosave: true = the fresh row existed and the fields were applied.
            return true;
          case 'get_fingerprint_count':
            return 1;
          case 'get_tracing_stats':
            return { total_spans: 2, failures: 0, total_duration_ms: 12.5, avg_duration_ms: 6.25 };
          case 'get_recent_spans':
            return [
              { operation: 'diff.compute', start: '0', duration_ms: 5.0, metadata: {}, success: true, error: null },
              { operation: 'asr.transcribe', start: '0', duration_ms: 7.5, metadata: {}, success: true, error: null },
            ];
          case 'clear_tracing_spans':
            return null;
          case 'transcribe_audio_with_scribe':
            return 'سکرایب کوردی';
          case 'add_scribe_votes':
            return 1;
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
                id: 'finetuned-mms-ckb',
                family: 'mms-ckb',
                model_card_name: 'MMS-CTC-1B (ckb)',
                checkpoint_sha256: 'a1b2c3d4e5f600112233445566778899aabbccddeeff00112233445566778899',
                checkpoint_path: '',
                source: 'fine-tune',
                license: 'CC-BY-NC-4.0',
                status: 'champion',
              },
              {
                id: 'omniasr-ctc-300m',
                family: 'omniasr',
                model_card_name: null,
                checkpoint_sha256: '00112233445566778899aabbccddeeffa1b2c3d4e5f6000000000000deadbeef',
                checkpoint_path: '',
                source: 'bundled',
                license: 'CC-BY-4.0',
                status: 'candidate',
              },
            ];
          case 'get_inference_stats':
            return {
              vad: { calls: 0, failures: 0, p50_ms: 0, p99_ms: 0 },
              asr: { calls: 0, failures: 0, p50_ms: 0, p99_ms: 0 },
              model_load_ms: 0,
            };
          case 'get_dataset_stats':
            return {
              totalSegments: 1,
              verifiedCount: 0,
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
              topSpeakers: [{ speakerId: 'SPEAKER_00', segmentCount: 1, totalDurationSeconds: 1.5 }],
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
          case 'delete_segments_batch':
          case 'export_huggingface_dataset':
            return null;
          case 'batch_verify':
          case 'batch_assign_speaker':
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
            return null;
        }
      },
    };

    (window as unknown as { __emitTauriEvent?: (event: string, payload: unknown) => void }).__emitTauriEvent = (
      event: string,
      payload: unknown,
    ) => {
      const ids = eventListenerIds.get(event) ?? [];
      for (const id of ids) {
        const cb = eventHandlers.get(id);
        cb?.({ event, id, payload });
      }
    };
  });
}

/** Emit a mocked Tauri event into the page (for progress UI tests). */
export async function emitTauriEvent(
  page: Page,
  event: string,
  payload: unknown,
): Promise<void> {
  await page.evaluate(
    ({ event, payload }) => {
      const w = window as unknown as { __emitTauriEvent?: (e: string, p: unknown) => void };
      w.__emitTauriEvent?.(event, payload);
    },
    { event, payload },
  );
}
