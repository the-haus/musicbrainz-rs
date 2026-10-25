// Types of the native addon (src/lib.rs). Everything structured crosses as
// JSON text; ts/index.ts parses it.

/** An event of a native task. `data` is JSON text. */
export interface NativeEvent {
  type: "progress" | "batch" | "member" | "end" | "error";
  data: string;
}

export declare class NativeTask {
  setRef(referenced: boolean): void;
  release(): void;
  request(n: number): void;
  close(): void;
}

export interface NativeOpenOptions {
  readOnly?: boolean;
  threads?: number;
  memoryLimit?: string;
}

export declare class NativeDatabase {
  static open(path: string, options?: NativeOpenOptions): NativeDatabase;
  query(sql: string, params?: string): Promise<string>;
  querySync(sql: string, params?: string): string;
  execute(sql: string, params?: string): Promise<string>;
  executeBatch(sql: string): void;
  describe(sql: string, params?: string): string;
  info(): string;
  stream(sql: string, params: string | undefined, batchSize: number, callback: (e: NativeEvent) => void): NativeTask;
  session(): NativeSession;
  close(): void;
}

export declare class NativeSession {
  query(sql: string, params?: string): Promise<string>;
  querySync(sql: string, params?: string): string;
  execute(sql: string, params?: string): Promise<string>;
  executeBatch(sql: string): void;
  describe(sql: string, params?: string): string;
  stream(sql: string, params: string | undefined, batchSize: number, callback: (e: NativeEvent) => void): NativeTask;
  close(): void;
}

type Callback = (e: NativeEvent) => void;

export declare function build(options: string, callback: Callback): NativeTask;
export declare function refresh(options: string, callback: Callback): NativeTask;
export declare function diff(oldPath: string, newPath: string, callback: Callback): NativeTask;
export declare function extract(archive: string, dir: string, options: string | undefined, callback: Callback): NativeTask;
export declare function download(options: string, callback: Callback): NativeTask;
export declare function latestExport(base?: string, userAgent?: string): Promise<string>;
export declare function exportChecksums(name: string, base?: string, userAgent?: string): Promise<string>;
export declare function archiveMembers(archive: string, threads?: number): Promise<string>;
export declare function exportInfo(path: string): Promise<string>;
export declare function schema(): string;
export declare function nativeVersion(): string;
