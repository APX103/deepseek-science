export const DSS_API_TOKEN_HEADER = "X-DSS-Token";

/** Merge caller headers with the per-launch API capability without mutating the input. */
export function withApiToken(
  token: string | undefined,
  headers?: HeadersInit,
): Headers {
  const merged = new Headers(headers);
  if (token) merged.set(DSS_API_TOKEN_HEADER, token);
  return merged;
}
