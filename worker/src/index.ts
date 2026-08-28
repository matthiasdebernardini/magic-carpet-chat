/**
 * Magic Carpet chat backend.
 *
 * One Durable Object per conversation id. The cell keeps the transcript in its
 * own SQLite table, forwards the whole transcript to the Anthropic Messages API,
 * streams the reply back byte for byte, and writes the reply to the table while
 * it arrives.
 *
 *   POST /chat/:id   {"message": "..."}  -> text/event-stream (the API's own SSE)
 *   GET  /chat/:id                       -> {"messages": [...]}
 *
 * One turn at a time per cell. A POST that arrives while a reply streams gets a
 * 409, because a second turn would read a history with two user messages in a
 * row and the Messages API rejects that for good.
 */

const MODEL = "claude-sonnet-5";
/** The reply ceiling. Streaming makes a large value safe; a small one truncates. */
const MAX_TOKENS = 64_000;
const ANTHROPIC_VERSION = "2023-06-01";
const DEFAULT_BASE_URL = "https://api.anthropic.com";
/** Write the growing reply at most this often. Per token would thrash the disk. */
const FLUSH_MS = 250;
/** …and at least this often, so a fast stream still leaves a recent row. */
const FLUSH_CHUNKS = 16;
/**
 * A turn that has taken no byte from the API for this long is dead, not slow,
 * and the next POST may have the cell. The API answers with message_start at
 * once and sends ping events through any pause, so a live stream refreshes the
 * clock far more often than this; only a turn whose runtime went away is quiet
 * for thirty seconds.
 */
const STALE_TURN_MS = 30_000;

export interface Env {
  CHAT: DurableObjectNamespace;
  /** celld has no secrets, only string vars. Never log this. */
  ANTHROPIC_API_KEY: string;
  /** Test hook: point the cell at a stub API. Unset in production. */
  ANTHROPIC_BASE_URL?: string;
}

interface StoredMessage extends Record<string, SqlStorageValue> {
  role: "user" | "assistant";
  content: string;
  created_at: number;
  /** 1 once the stream reached message_stop. 0 means the text is a fragment. */
  complete: number;
}

const ID = /^\/chat\/([A-Za-z0-9._-]{1,128})$/;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    // No auth on purpose: the tailnet is the perimeter. Put authentication in
    // front of this worker before you expose it on the public internet.
    const match = ID.exec(new URL(request.url).pathname);
    if (!match) return json({ error: "not found" }, 404);
    if (request.method !== "GET" && request.method !== "POST") {
      return json({ error: "method not allowed" }, 405);
    }
    return env.CHAT.get(env.CHAT.idFromName(match[1])).fetch(request);
  },
};

export class ChatCell {
  private sql: SqlStorage;
  /**
   * True from the moment a turn is accepted until its relay stops. In memory on
   * purpose: an evicted cell has nothing in flight, so a fresh instance starts
   * false and is right.
   */
  private inFlight = false;
  /** When the running turn last took a byte from the API. See STALE_TURN_MS. */
  private progressAt = 0;
  /**
   * Counts claims. A relay writes only while it is still the current turn, so a
   * stream declared stale and taken over cannot come back to life and append a
   * second assistant message after the new turn's.
   */
  private turn = 0;

  constructor(
    private ctx: DurableObjectState,
    private env: Env,
  ) {
    this.sql = ctx.storage.sql;
    this.sql.exec(
      `CREATE TABLE IF NOT EXISTS messages (
         id         INTEGER PRIMARY KEY,
         role       TEXT NOT NULL,
         content    TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         complete   INTEGER NOT NULL DEFAULT 1
       )`,
    );
    // A cell written before the complete flag existed still has the old table.
    const columns = this.sql.exec<{ name: string }>("PRAGMA table_info(messages)").toArray();
    if (!columns.some((column) => column.name === "complete")) {
      this.sql.exec("ALTER TABLE messages ADD COLUMN complete INTEGER NOT NULL DEFAULT 1");
    }
  }

  async fetch(request: Request): Promise<Response> {
    if (request.method === "GET") {
      return json({
        messages: this.history().map(({ role, content, created_at, complete }) => ({
          role,
          content,
          created_at,
          complete: complete === 1,
        })),
      });
    }

    let body: unknown;
    try {
      body = await request.json();
    } catch {
      return json({ error: "the body must be JSON" }, 400);
    }
    const message = (body as { message?: unknown } | null)?.message;
    if (typeof message !== "string" || message.trim() === "") {
      return json({ error: "message must be a non-empty string" }, 400);
    }

    // Claim the cell. Nothing may await between the test and the set, or two
    // POSTs both pass it and the transcript ends up with two user turns in a
    // row — a history the Messages API refuses from then on.
    //
    // A client that hangs up mid-stream takes the relay's whole execution
    // context down with it: the write to the client never settles, so the catch
    // and the finally below it never run. The flag alone would then stay set for
    // the life of the cell, so a silent turn also loses its claim.
    if (this.inFlight && Date.now() - this.progressAt < STALE_TURN_MS) {
      return json({ error: "a reply is in flight" }, 409);
    }
    this.inFlight = true;
    this.progressAt = Date.now();
    const turn = ++this.turn;

    // The last turn may have died before its reply, leaving the user message
    // unanswered. Two user messages in a row is the one history the Messages API
    // refuses, so take the orphan back first. In the normal case the last row is
    // an assistant message and this does nothing.
    const last = this.sql
      .exec<{ id: number; role: string }>("SELECT id, role FROM messages ORDER BY id DESC LIMIT 1")
      .toArray()[0];
    if (last?.role === "user") this.sql.exec("DELETE FROM messages WHERE id = ?", last.id);

    const userRow = this.sql
      .exec<{ id: number }>(
        "INSERT INTO messages (role, content, created_at) VALUES ('user', ?, ?) RETURNING id",
        message,
        Date.now(),
      )
      .one().id;

    const base = this.env.ANTHROPIC_BASE_URL ?? DEFAULT_BASE_URL;
    let upstream: Response;
    try {
      upstream = await fetch(`${base}/v1/messages`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "text/event-stream",
          "anthropic-version": ANTHROPIC_VERSION,
          "x-api-key": this.env.ANTHROPIC_API_KEY ?? "",
        },
        body: JSON.stringify({
          model: MODEL,
          max_tokens: MAX_TOKENS,
          stream: true,
          messages: this.history().map(({ role, content }) => ({ role, content })),
        }),
      });
    } catch (error) {
      // Roll the turn back so the transcript never ends on an unanswered user
      // message; a retry then sends one user turn, not two.
      this.sql.exec("DELETE FROM messages WHERE id = ?", userRow);
      if (this.turn === turn) this.inFlight = false;
      return json({ error: `could not reach the Anthropic API: ${describe(error)}` }, 502);
    }

    if (!upstream.ok || upstream.body === null) {
      this.sql.exec("DELETE FROM messages WHERE id = ?", userRow);
      if (this.turn === turn) this.inFlight = false;
      return new Response(await upstream.text(), {
        status: upstream.status,
        headers: {
          "content-type": upstream.headers.get("content-type") ?? "application/json",
        },
      });
    }

    const { readable, writable } = new TransformStream<Uint8Array, Uint8Array>();
    // Not awaited. relay() writes the reply to the table while it arrives, so a
    // client that hangs up mid-stream keeps whatever text already landed,
    // flagged incomplete. waitUntil does not hold the relay open on workerd —
    // the incremental write is what saves the turn, not this call.
    this.ctx.waitUntil(this.relay(upstream.body, writable, userRow, turn));
    return new Response(readable, {
      headers: { "content-type": "text/event-stream", "cache-control": "no-store" },
    });
  }

  private history(): StoredMessage[] {
    return this.sql
      .exec<StoredMessage>(
        "SELECT role, content, created_at, complete FROM messages ORDER BY id",
      )
      .toArray();
  }

  /**
   * Copy the API's SSE to the client and write the reply to the table while it
   * arrives. The assistant row appears on the first text delta and grows from
   * there, so a client hangup, an upstream death, or an evicted cell leaves the
   * text that already came through instead of nothing. The row carries
   * complete = 0 until message_stop.
   */
  private async relay(
    upstream: ReadableStream<Uint8Array>,
    writable: WritableStream<Uint8Array>,
    userRow: number,
    turn: number,
  ): Promise<void> {
    let reply = "";
    let assistantRow: number | null = null;
    let stored = "";
    let storedComplete = false;
    let lastFlush = 0;
    let sinceFlush = 0;

    /** Put the reply so far on disk. The first call inserts, the rest update. */
    const save = (complete: boolean): void => {
      if (reply === "" || this.turn !== turn) return;
      if (assistantRow === null) {
        assistantRow = this.sql
          .exec<{ id: number }>(
            `INSERT INTO messages (role, content, created_at, complete)
             VALUES ('assistant', ?, ?, ?) RETURNING id`,
            reply,
            Date.now(),
            complete ? 1 : 0,
          )
          .one().id;
      } else {
        this.sql.exec(
          "UPDATE messages SET content = ?, complete = ? WHERE id = ?",
          reply,
          complete ? 1 : 0,
          assistantRow,
        );
      }
      stored = reply;
      storedComplete = complete;
      lastFlush = Date.now();
      sinceFlush = 0;
    };

    try {
      const reader = upstream.getReader();
      const writer = writable.getWriter();
      const decoder = new TextDecoder();
      let rest = "";
      let stopped = false;

      /** Fold one SSE line into the reply, flushing on the batch rules. */
      const take = (line: string): void => {
        const event = parse(line);
        if (event === null) return;
        if (event.type === "message_stop") {
          stopped = true;
          return;
        }
        if (event.type !== "content_block_delta" || event.delta?.type !== "text_delta") return;
        reply += event.delta.text ?? "";
        sinceFlush += 1;
        // The first delta always lands: that is what makes a hangup survivable.
        if (assistantRow === null || sinceFlush >= FLUSH_CHUNKS || Date.now() - lastFlush >= FLUSH_MS) {
          save(false);
        }
      };

      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          // Proof this turn is still alive, for the staleness test in fetch().
          if (this.turn === turn) this.progressAt = Date.now();
          await writer.write(value);
          rest += decoder.decode(value, { stream: true });
          let cut: number;
          while ((cut = rest.indexOf("\n")) !== -1) {
            take(rest.slice(0, cut));
            rest = rest.slice(cut + 1);
          }
        }
        take(rest + decoder.decode());
        await writer.close();
      } catch (error) {
        // Cancel the reader too, or a dead client leaves the API connection open.
        await reader.cancel(error).catch(() => {});
        await writer.abort(error).catch(() => {});
      }

      if (reply === "") {
        // The turn produced no text: an upstream error event, a reply of tool
        // use or thinking alone, or a stream that broke before the first delta.
        // Take the user turn back. The Messages API rejects a history whose
        // roles do not alternate, so an unanswered user turn wedges the cell.
        if (this.turn === turn) this.sql.exec("DELETE FROM messages WHERE id = ?", userRow);
        return;
      }
      // The tail of the text, and the verdict on whether the turn finished.
      if (stored !== reply || storedComplete !== stopped) save(stopped);
    } finally {
      // Every ending the runtime lets us see: a finished stream, a dead
      // upstream, an error event. A client hangup is the one it does not — that
      // kills this whole context mid-await, and STALE_TURN_MS covers it.
      // Only the current turn may release the cell; a superseded relay that
      // finishes late must not hand away the claim its successor holds.
      if (this.turn === turn) this.inFlight = false;
    }
  }
}

interface SseEvent {
  type?: string;
  delta?: { type?: string; text?: string };
}

/** The parsed payload of one SSE `data:` line, or null for every other line. */
function parse(line: string): SseEvent | null {
  const trimmed = line.trimEnd();
  if (!trimmed.startsWith("data:")) return null;
  try {
    const event: unknown = JSON.parse(trimmed.slice(5));
    return typeof event === "object" && event !== null ? (event as SseEvent) : null;
  } catch {
    return null;
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}
