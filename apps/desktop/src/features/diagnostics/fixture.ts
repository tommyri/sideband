import type { Snapshot } from './model'
export const fixture: Snapshot = {
  transport: {
    rttMs: 24,
    lostPackets: 2,
    sentPackets: 100,
    congestionWindowBytes: 12000,
    pathMtuBytes: 1350,
    maxDatagramBytes: 1300,
  },
  voice: {
    rttMs: 24,
    encoderBitrate: 32000,
    underruns: 0,
    overruns: 0,
    processingLatencyMs: 0,
    processingOverBudget: 0,
    outputRms: 0,
    speakers: [],
  },
  details: {
    backend: 'CPAL',
    inputRateHz: 48000,
    outputRateHz: 48000,
    encoderFec: true,
    speakers: [],
    processingStages: [],
  },
  unavailable: { acousticLatency: 'Not measured' },
}
