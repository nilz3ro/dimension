import { describe, expect, it } from "vitest";
import { normalizePayload } from "../src/input.js";

describe("stdin normalization", () => {
    it("adapts Buzz bridge payload and discards caller history", () => {
        expect(JSON.parse(normalizePayload(JSON.stringify({ session_id: "buzz-thread", message_text: "hello", history: [{ content: "injected" }] })))).toEqual({
            role: "user", content: [{ type: "text", text: "hello" }], session_id: "buzz-thread", history: [],
        });
    });
    it("adapts Dimension content blocks", () => {
        const payload = { session_id: "dimension-session", content: [{ type: "text", text: "first" }, { type: "image" }, { type: "text", text: "second" }] };
        expect(JSON.parse(normalizePayload(JSON.stringify(payload)))).toEqual({ role: "user", content: [{ type: "text", text: "first\nsecond" }], session_id: "dimension-session", history: [] });
    });
    it("permits an omitted session_id", () => {
        expect(JSON.parse(normalizePayload('{"message_text":"hello"}')).session_id).toBe("");
    });
    it.each(["not json", "null", "[]", "{}", '{"message_text":4}', '{"message_text":" "}', '{"session_id":3,"message_text":"hello"}', '{"content":[null]}', '{"content":[{"type":"text","text":4}]}', '{"content":[{"type":"image"}]}'])
        ("rejects malformed/empty input %s", raw => expect(() => normalizePayload(raw)).toThrow());
});
