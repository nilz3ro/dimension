import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { Readable } from "node:stream";

// ── Module-level mocks ──────────────────────────────────────────────────────

// Mock the Agent class from pi-agent-core
vi.mock("@mariozechner/pi-agent-core", () => {
    return {
        Agent: vi.fn().mockImplementation(() => ({
            subscribe: vi.fn(),
            prompt: vi.fn(),
        })),
    };
});

// Mock pi-ai getModel
vi.mock("@mariozechner/pi-ai", () => ({
    getModel: vi.fn(() => ({ id: "test-model" })),
}));

// Mock the compat model builder
vi.mock("../src/model.js", () => ({
    createCompatModel: vi.fn(() => ({ id: "compat-model" })),
}));

// ── Test utilities ──────────────────────────────────────────────────────────

function makeDimensionRequest(overrides: Record<string, unknown> = {}) {
    return {
        role: "user",
        content: [{ type: "text", text: "hello agent" }],
        session_id: "test-session-42",
        bundle_id: "coder",
        history: [],
        truncation: { total_messages: 1, included_messages: 1, truncated: false },
        ...overrides,
    };
}

/**
 * Replace process.stdin with a Readable that emits `data` then `end`.
 * Returns a restore function.
 */
function fakeStdin(data: string): () => void {
    const original = process.stdin;
    const stream = new Readable({
        read() {
            this.push(Buffer.from(data));
            this.push(null); // signal end
        },
    });
    Object.defineProperty(process, "stdin", { value: stream, writable: true, configurable: true });
    return () => {
        Object.defineProperty(process, "stdin", { value: original, writable: true, configurable: true });
    };
}

// ── Tests ────────────────────────────────────────────────────────────────────

describe("runAgent history handling", () => {
    let exitSpy: ReturnType<typeof vi.spyOn>;
    let stdoutSpy: ReturnType<typeof vi.spyOn>;
    let stderrSpy: ReturnType<typeof vi.spyOn>;
    let restoreStdin: (() => void) | undefined;

    beforeEach(() => {
        // Prevent process.exit from killing the test runner
        exitSpy = vi.spyOn(process, "exit").mockImplementation((() => {}) as any);
        stdoutSpy = vi.spyOn(process.stdout, "write").mockImplementation(() => true);
        stderrSpy = vi.spyOn(process.stderr, "write").mockImplementation(() => true);
    });

    afterEach(() => {
        exitSpy.mockRestore();
        stdoutSpy.mockRestore();
        stderrSpy.mockRestore();
        restoreStdin?.();
        restoreStdin = undefined;
    });

    it("passes request-payload history to the Agent as initial messages", async () => {
        const { Agent } = await import("@mariozechner/pi-agent-core");
        let capturedConfig: any = null;
        let subscribeCb: ((e: any) => void) | null = null;

        (Agent as unknown as ReturnType<typeof vi.fn>).mockImplementation((cfg: any) => {
            capturedConfig = cfg;
            return {
                subscribe: vi.fn((cb: any) => { subscribeCb = cb; }),
                prompt: vi.fn(() => {
                    if (subscribeCb) {
                        subscribeCb({
                            type: "agent_end",
                            messages: [{ role: "assistant", content: "done" }],
                        });
                    }
                }),
            };
        });

        const req = makeDimensionRequest({
            history: [
                { role: "user", content: "old message", timestamp: "2026-01-01T00:00:00Z" },
            ],
        });
        restoreStdin = fakeStdin(JSON.stringify(req));

        const { runAgent } = await import("../src/agent.js");
        await runAgent({ systemPrompt: "test", tools: [] });

        // Agent should have received the request-provided history
        expect(capturedConfig.initialState.messages).toHaveLength(1);
        expect(capturedConfig.initialState.messages[0].role).toBe("user");
    });

    it("writes the final assistant text to stdout on agent_end", async () => {
        const { Agent } = await import("@mariozechner/pi-agent-core");
        const finalMessages = [
            { role: "user", content: [{ type: "text", text: "hello" }] },
            { role: "assistant", content: [{ type: "text", text: "world" }] },
        ];
        let subscribeCb: ((e: any) => void) | null = null;

        (Agent as unknown as ReturnType<typeof vi.fn>).mockImplementation(() => ({
            subscribe: vi.fn((cb: any) => { subscribeCb = cb; }),
            prompt: vi.fn(() => {
                if (subscribeCb) {
                    subscribeCb({ type: "agent_end", messages: finalMessages });
                }
            }),
        }));

        const req = makeDimensionRequest();
        restoreStdin = fakeStdin(JSON.stringify(req));

        const { runAgent } = await import("../src/agent.js");
        await runAgent({ systemPrompt: "test", tools: [] });

        expect(stdoutSpy).toHaveBeenCalledWith("world");
    });
});
