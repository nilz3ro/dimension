import { afterEach, describe, expect, it, vi } from "vitest";
import { createChatModel } from "../src/model.js";

afterEach(() => vi.unstubAllEnvs());

describe("model configuration", () => {
    it.each(["http://localhost:8000", "http://localhost:8000/v1/"])("normalizes %s without changing origin", base => {
        vi.stubEnv("MODEL_BASE_URL", base);
        vi.stubEnv("MODEL_NAME", "test-model");
        vi.stubEnv("OPENAI_API_KEY", "ambient-key-do-not-use");
        const model = createChatModel();
        expect(model.baseUrl).toBe("http://localhost:8000/v1");
        expect(model.id).toBe("test-model");
        expect(model.headers).toEqual({});
        expect(process.env.OPENAI_API_KEY).toBe("ambient-key-do-not-use");
    });
    it.each(["", "file:///tmp/model", "http://user:password@localhost", "http://localhost?q=secret", "http://localhost#fragment"])("rejects invalid base %s", base => {
        vi.stubEnv("MODEL_BASE_URL", base);
        vi.stubEnv("MODEL_NAME", "test-model");
        expect(() => createChatModel()).toThrow();
    });
    it("fails closed on missing model name", () => {
        vi.stubEnv("MODEL_BASE_URL", "http://localhost:8000/v1");
        vi.stubEnv("MODEL_NAME", "");
        expect(() => createChatModel()).toThrow("required");
    });
});
