// Native tasks as Promises (builds, downloads, extractions, diffs) and as
// async iterables (streamed queries).
//
// A task runs on a native thread and calls back with events. A Promise
// task keeps the event loop alive until it ends. A row stream takes one
// batch per credit it grants, so at most `readAhead` batches wait in
// memory however slow the consumer; it keeps the event loop alive only
// while something awaits a batch, and one dropped half-read is closed when
// collected.

import type { NativeEvent, NativeTask } from "./native.js";
import { MusicBrainzError } from "./types.js";

type Start = (callback: (e: NativeEvent) => void) => NativeTask;

function errorOf(data: string): MusicBrainzError {
  let message = data;
  try {
    message = JSON.parse(data) as string;
  } catch {
    /* plain text */
  }
  return new MusicBrainzError(message === "stopped" ? "aborted" : message, message === "stopped" ? "aborted" : "failed");
}

/** Options every task takes. */
export interface TaskOptions<P> {
  /** Progress reports, about every 250 ms (dropped while JS is busy). */
  onProgress?: ((progress: P) => void) | undefined;
  /** Stops the task: the Promise rejects with an `aborted` error. */
  signal?: AbortSignal | undefined;
}

/**
 * Runs a native task to its end. Its `end` event's data is the result;
 * `member` events (extractions) go to `onEvent`.
 */
export function runTask<T, P>(
  start: Start,
  options: TaskOptions<P> = {},
  onEvent?: (type: string, data: unknown) => void,
): Promise<T> {
  const { onProgress, signal } = options;
  if (signal?.aborted) return Promise.reject(new MusicBrainzError("aborted", "aborted"));
  return new Promise<T>((resolve, reject) => {
    let native: NativeTask | undefined;
    const abort = () => native?.close();
    const finish = () => {
      signal?.removeEventListener("abort", abort);
      native?.release();
    };
    native = start((e) => {
      switch (e.type) {
        case "progress":
          if (onProgress) {
            try {
              onProgress(JSON.parse(e.data) as P);
            } catch {
              /* a throwing callback must not stop the task */
            }
          }
          break;
        case "end":
          finish();
          if (signal?.aborted) reject(new MusicBrainzError("aborted", "aborted"));
          else resolve(JSON.parse(e.data) as T);
          break;
        case "error":
          finish();
          reject(errorOf(e.data));
          break;
        default:
          onEvent?.(e.type, JSON.parse(e.data));
      }
    });
    signal?.addEventListener("abort", abort, { once: true });
  });
}

const pinned = new Set<object>();
const finalizer = new FinalizationRegistry<NativeTask>((n) => {
  try {
    n.close();
    n.setRef(false);
    n.release();
  } catch {
    /* already released */
  }
});

/** What a finished row stream read. */
export interface StreamStats {
  /** Rows read (sent to JS, or skipped once closed). */
  rows: number;
  /** Whether it was closed before its end. */
  stopped: boolean;
}

/** Options of {@link RowStream}. */
export interface StreamOptions {
  /** Rows per batch (default 5,000). */
  batchSize?: number;
  /** Batches read ahead of the consumer (default 2). */
  readAhead?: number;
  /** Closes the stream; iteration then throws an `aborted` error. */
  signal?: AbortSignal;
}

/**
 * A query's rows, read as they come. Iterate it once, row by row
 * (`for await (const row of stream)`) or batch by batch
 * (`stream.batches()`). Nothing runs until iteration starts; breaking out
 * of the loop (or `close()`) stops the query.
 */
export class RowStream<T> implements AsyncIterable<T> {
  /** Resolves when the stream ended or was closed. */
  readonly stats: Promise<StreamStats>;
  #start: Start;
  #native: NativeTask | undefined;
  #queue: string[] = [];
  #waiter: { resolve: (b: T[] | null) => void; reject: (e: Error) => void } | undefined;
  #ended = false;
  #error: Error | undefined;
  #claimed = false;
  #referenced = true;
  #readAhead: number;
  #signal: AbortSignal | undefined;
  #resolveStats!: (s: StreamStats) => void;

  /** @internal */
  constructor(start: Start, options: StreamOptions = {}) {
    this.#start = start;
    this.#readAhead = Math.max(1, options.readAhead ?? 2);
    this.#signal = options.signal;
    this.stats = new Promise((resolve) => (this.#resolveStats = resolve));
  }

  #updateRef(): void {
    if (!this.#native || this.#ended) return;
    const wanted = this.#waiter !== undefined;
    if (wanted) pinned.add(this);
    else pinned.delete(this);
    if (wanted !== this.#referenced) {
      this.#referenced = wanted;
      this.#native.setRef(wanted);
    }
  }

  #onEvent(e: NativeEvent): void {
    switch (e.type) {
      case "batch":
        if (this.#waiter) {
          const w = this.#waiter;
          this.#waiter = undefined;
          this.#native?.request(1);
          w.resolve(JSON.parse(e.data) as T[]);
          this.#updateRef();
        } else {
          this.#queue.push(e.data);
        }
        break;
      case "end":
      case "error":
        this.#ended = true;
        pinned.delete(this);
        if (this.#native) {
          finalizer.unregister(this);
          this.#native.release();
        }
        if (e.type === "error") this.#error = errorOf(e.data);
        this.#resolveStats(e.type === "end" ? (JSON.parse(e.data) as StreamStats) : { rows: 0, stopped: true });
        if (this.#waiter) {
          const w = this.#waiter;
          this.#waiter = undefined;
          if (this.#error) w.reject(this.#error);
          else w.resolve(null);
        }
        break;
    }
  }

  #begin(): void {
    if (this.#claimed) throw new MusicBrainzError("a stream is iterated once", "invalid");
    this.#claimed = true;
    if (this.#signal?.aborted) {
      this.#ended = true;
      this.#error = new MusicBrainzError("aborted", "aborted");
      this.#resolveStats({ rows: 0, stopped: true });
      return;
    }
    this.#signal?.addEventListener("abort", () => {
      this.#error ??= new MusicBrainzError("aborted", "aborted");
      this.close();
    });
    this.#native = this.#start((e) => this.#onEvent(e));
    finalizer.register(this, this.#native, this);
    this.#native.request(this.#readAhead);
  }

  /** The next batch, or null at the end. */
  #next(): Promise<T[] | null> {
    const queued = this.#queue.shift();
    if (queued !== undefined) {
      this.#native?.request(1);
      return Promise.resolve(JSON.parse(queued) as T[]);
    }
    if (this.#error) return Promise.reject(this.#error);
    if (this.#ended) return Promise.resolve(null);
    return new Promise((resolve, reject) => {
      this.#waiter = { resolve, reject };
      this.#updateRef();
    });
  }

  /** The rows, a batch at a time. */
  async *batches(): AsyncGenerator<T[], void, undefined> {
    this.#begin();
    try {
      for (;;) {
        const batch = await this.#next();
        if (batch === null) return;
        if (batch.length) yield batch;
      }
    } finally {
      this.close();
    }
  }

  async *[Symbol.asyncIterator](): AsyncIterator<T> {
    for await (const batch of this.batches()) yield* batch;
  }

  /** Every row, in one array. */
  async toArray(): Promise<T[]> {
    const out: T[] = [];
    for await (const batch of this.batches()) out.push(...batch);
    return out;
  }

  /** Stops the query; no more batches come. */
  close(): void {
    if (this.#ended || !this.#native) return;
    this.#queue = [];
    this.#native.close();
    if (!this.#referenced) {
      this.#referenced = true;
      this.#native.setRef(true);
    }
  }
}
