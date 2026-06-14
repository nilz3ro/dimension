//! Outbound message API for bundles running inside a Dimension VM.
//!
//! `dimension.send({...})` writes a length-prefixed JSON frame to the
//! Unix-domain socket exposed by `dimension-agent` at the path given by
//! the `DIMENSION_EVENTS_SOCK` environment variable. The agent then
//! forwards each frame as a protobuf `OutboundMessage` envelope to the
//! worker, which fans the event out to Clickhouse, Pulsar, and the
//! gateway's SSE stream.
//!
//! Outside the VM (env var unset) the calls are no-ops so the same
//! bundle code can run locally for development.

import { Buffer } from "node:buffer";
import { randomUUID } from "node:crypto";
import { connect, type Socket } from "node:net";

export interface DimensionEvent {
    /** Free-form discriminator. Defaults to `"bundle"`. */
    kind?: string;
    /** MIME-style content type. Defaults to `"application/json"`. */
    contentType?: string;
    /**
     * Event body. Strings are sent as UTF-8 bytes; everything else is
     * JSON-stringified.
     */
    body: unknown;
    /** Free-form string tags. */
    attrs?: Record<string, string>;
}

const SOCK_ENV = "DIMENSION_EVENTS_SOCK";
const MAX_FRAME = 256 * 1024;

let cachedSocket: Socket | null = null;
let connecting: Promise<Socket | null> | null = null;

function sockPath(): string | undefined {
    const p = process.env[SOCK_ENV];
    return p && p.length > 0 ? p : undefined;
}

async function getSocket(): Promise<Socket | null> {
    const path = sockPath();
    if (!path) return null;
    if (cachedSocket && !cachedSocket.destroyed) return cachedSocket;
    if (connecting) return connecting;

    connecting = new Promise<Socket | null>((resolve) => {
        const s = connect(path);
        const onError = (err: Error) => {
            process.stderr.write(`[dimension] events socket connect failed: ${err.message}\n`);
            cachedSocket = null;
            resolve(null);
        };
        s.once("error", onError);
        s.once("connect", () => {
            s.off("error", onError);
            s.on("error", (e) => {
                process.stderr.write(`[dimension] events socket error: ${e.message}\n`);
                cachedSocket = null;
            });
            s.on("close", () => {
                cachedSocket = null;
            });
            cachedSocket = s;
            resolve(s);
        });
    });

    try {
        return await connecting;
    } finally {
        connecting = null;
    }
}

function encodeBody(body: unknown): string {
    if (typeof body === "string") return body;
    return JSON.stringify(body);
}

/**
 * Send an event to the host. Resolves once the frame has been handed off
 * to the kernel (subject to TCP-style backpressure on the underlying
 * socket). Errors are logged to stderr and otherwise swallowed so a
 * misbehaving sink never interrupts the bundle.
 */
export async function send(event: DimensionEvent): Promise<void> {
    const socket = await getSocket();
    if (!socket) return; // dev mode or socket unavailable

    const frame = {
        kind: event.kind ?? "bundle",
        contentType: event.contentType ?? "application/json",
        body: typeof event.body === "string" ? event.body : event.body,
        attrs: event.attrs ?? {},
    };
    const json = Buffer.from(
        JSON.stringify({
            kind: frame.kind,
            contentType: frame.contentType,
            body: frame.body,
            attrs: frame.attrs,
        }),
    );
    if (json.length > MAX_FRAME) {
        process.stderr.write(
            `[dimension] send: body too large (${json.length} > ${MAX_FRAME})\n`,
        );
        return;
    }
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32LE(json.length, 0);

    await new Promise<void>((resolve) => {
        const ok = socket.write(header);
        if (!ok) {
            socket.once("drain", () => {
                socket.write(json, () => resolve());
            });
        } else {
            socket.write(json, () => resolve());
        }
    });
}

/** Returns true when the bundle is running inside a Dimension VM and the
 *  outbound socket is reachable.
 */
export function isConnected(): boolean {
    return Boolean(cachedSocket) && !cachedSocket?.destroyed;
}

/**
 * Emit a bridge-consumable event. This is the canonical way for a bundle to
 * deliver messages, documents, and progress: the frame travels
 * UDS → vsock → worker → SSE, where consumers (or bridge apps such as
 * telegram-bridge) receive it as a `bundle` event whose body has this shape.
 *
 * `event_type` semantics understood by bridges:
 *   "Message"  — assistant text, forwarded to the end user
 *   "Document" — content is JSON { url, filename?, caption? }
 *   "ToolCall" / "ToolResult" — informational, not forwarded
 */
export async function sendMessage(
    content: string,
    opts?: { eventType?: string; role?: string; sessionId?: string },
): Promise<void> {
    await send({
        body: {
            session_id: opts?.sessionId ?? process.env["CONVERSATION_ID"] ?? "",
            event_type: opts?.eventType ?? "Message",
            event_id: randomUUID(),
            content: { role: opts?.role ?? "assistant", content },
            timestamp: new Date().toISOString(),
        },
    });
}

/** Default namespace export so callers can write `dimension.send(...)`. */
export const dimension = { send, isConnected, sendMessage };

void encodeBody; // reserved for future byte-only payloads
