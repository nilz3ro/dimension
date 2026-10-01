/** Normalize Buzz bridge or Dimension text payloads into a single stateless turn. */
export function normalizePayload(raw: string): string {
    const payload: unknown = JSON.parse(raw);
    if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
        throw new Error("Expected a JSON object payload");
    }
    const req = payload as Record<string, unknown>;
    if (req.session_id !== undefined && typeof req.session_id !== "string") {
        throw new Error("session_id must be a string");
    }

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

    // Each VM turn is stateless for this proof. Do not accept caller-supplied history.
    return JSON.stringify({
        role: "user",
        content: [{ type: "text", text }],
        session_id: req.session_id ?? "",
        history: [],
    });
}
