// Only `models` is consumed (modelRuntimeDefaults test); dead fields removed in the 2026-07-15 audit.
export const APP_CONFIG = {
  models: {
    omniasrModel: 'models/omniasr-ctc-300m/model.int8.onnx',
    omniasrTokens: 'models/omniasr-ctc-300m/tokens.txt',
    vadPath: 'models/silero_vad_v4.onnx',
  },
};
