// Native commands reject with an `ApiError` object, not an `Error`, so test
// for a message field rather than `instanceof Error`.
export function errorText(error: unknown, fallback = 'The request failed. Please try again.') {
  return error && typeof error === 'object' && 'message' in error && error.message
    ? String(error.message)
    : fallback
}
