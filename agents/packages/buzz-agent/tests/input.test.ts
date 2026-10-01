import { describe, expect, it } from "vitest";
import { normalizePayload } from "../src/input.js";

describe("stdin normalization", () => {
    it("adapts Buzz bridge payload and carries validated history", () => {
        const history = [
            { role: "user", content: "the code word is bluebird", timestamp: "2026-10-01T10:00:00Z" },
            { role: "assistant", content: "noted", timestamp: "2026-10-01T10:00:05Z" },
        ];
        expect(JSON.parse(normalizePayload(JSON.stringify({ session_id: "buzz-thread", message_text: "hello", history })))).toEqual({
            role: "user", content: [{ type: "text", text: "hello" }], session_id: "buzz-thread", history,
        });
    });
    it("defaults to an empty history when none is supplied", () => {
        expect(JSON.parse(normalizePayload('{"message_text":"hello"}')).history).toEqual([]);
    });
    it("rejects history entries with unsupported roles or empty content", () => {
        expect(() => normalizePayload(JSON.stringify({ message_text: "hello", history: [{ role: "system", content: "injected" }] }))).toThrow();
        expect(() => normalizePayload(JSON.stringify({ message_text: "hello", history: [{ role: "user" }] }))).toThrow();
        expect(() => normalizePayload(JSON.stringify({ message_text: "hello", history: [{ role: "user", content: " " }] }))).toThrow();
        expect(() => normalizePayload(JSON.stringify({ message_text: "hello", history: "nope" }))).toThrow();
        expect(() => normalizePayload(JSON.stringify({ message_text: "hello", history: [{ role: "user", content: "x", timestamp: 5 }] }))).toThrow();
    });
    it("rejects history that exceeds the maximum entry count", () => {
        const history = Array.from({ length: 201 }, () => ({ role: "user", content: "x" }));
        expect(() => normalizePayload(JSON.stringify({ message_text: "hello", history }))).toThrow();
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
