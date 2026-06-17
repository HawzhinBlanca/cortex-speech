export const APP_CONFIG = {
  version: '2.0.0',
  name: 'Cortex Speech',
  locale: 'ckb',
  defaults: {
    sampleRate: 16000,
    maxTranscriptLength: 100000,
    autoSaveIntervalMs: 60000,
    maxUndoHistory: 500,
  },
  models: {
    omniasrModel: 'models/omniasr-ctc-300m/model.int8.onnx',
    omniasrTokens: 'models/omniasr-ctc-300m/tokens.txt',
    vadPath: 'models/silero_vad_v4.onnx',
  },
};
