/**
 * Discards stale async reads after a newer read or a live mutation starts.
 * Keys are independent so switching sessions does not invalidate unrelated
 * background loads.
 */
export class AsyncVersionGuard {
  private readonly versions = new Map<string, number>()

  begin(key: string): number {
    const version = (this.versions.get(key) ?? 0) + 1
    this.versions.set(key, version)
    return version
  }

  invalidate(key: string): void {
    this.begin(key)
  }

  isCurrent(key: string, version: number): boolean {
    return this.versions.get(key) === version
  }
}
