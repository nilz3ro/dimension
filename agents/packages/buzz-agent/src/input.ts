/** Normalize Buzz bridge or Dimension text payloads into a single turn with bounded history. */
export interface HistoryEntry {
    role: "user" | "assistant";
    content: string;
    timestamp?: string;
}

const MAX_HISTORY_ENTRIES = 200;

/** Validate caller-supplied history: user/assistant text turns only. */
export function parseHistory(value: unknown): HistoryEntry[] {
    if (value === undefined) return [];
    if (!Array.isArray(value)) throw new Error("history must be an array");
    if (value.length > MAX_HISTORY_ENTRIES) throw new Error("history exceeds the maximum entry count");
    return value.map((entry: unknown): HistoryEntry => {
        if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
            throw new Error("Invalid history entry");
        }
        const record = entry as Record<string, unknown>;
        if (record.role !== "user" && record.role !== "assistant") {
            throw new Error("history entries must be user or assistant turns");
        }
        if (typeof record.content !== "string" || !record.content.trim()) {
            throw new Error("history content must be a non-empty string");
        }
        if (record.timestamp !== undefined && typeof record.timestamp !== "string") {
            throw new Error("history timestamp must be a string when present");
        }
        return { role: record.role, content: record.content, timestamp: record.timestamp };
    });
}

export function normalizePayload(raw: string): string {
    const payload: unknown = JSON.parse(raw);
    if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
        throw new Error("Expected a JSON object payload");
    }
    const req = payload as Record<string, unknown>;
    if (req.session_id !== undefined && typeof req.session_id !== "string") {
        throw new Error("session_id must be a string");
    }
    const history = parseHistory(req.history);

    let text: string;
    if (req.message_text !== undefined) {
        if (typeof req.message_text !== "string") {
            throw new Error("message_text must be a string");
        }
        text = req.message_text;
    } else if (Array.isArray(req.content)) {
        text = req.content.map((block: unknown) => {
            if (!block || typeof block !== "object") {
                throw new Error("Invalid content block");
            }
            const content = block as Record<string, unknown>;
            if (content.type !== "text") return "";
            if (typeof content.text !== "string") {
                throw new Error("Text content must contain a string text field");
            }
            return content.text;
        }).filter(Boolean).join("\n");
    } else {
        throw new Error("Expected message_text or Dimension content blocks");
    }
    if (!text.trim()) throw new Error("No user message found");

    // History arrives pre-bounded and validated from the bridge; pass it
    // through so the model sees the session transcript.
    return JSON.stringify({
        role: "user",
        content: [{ type: "text", text }],
        session_id: req.session_id ?? "",
        history,
    });
}
