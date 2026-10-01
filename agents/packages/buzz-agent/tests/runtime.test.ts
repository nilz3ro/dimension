import { spawn } from "node:child_process";
import { createServer, type IncomingHttpHeaders } from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, describe, expect, it } from "vitest";

const cleanup: (() => Promise<void>)[] = [];
afterEach(async () => { for (const close of cleanup.splice(0)) await close(); });

async function mockEndpoint(status = 200, response: unknown = { choices: [{ finish_reason: "stop", message: { role: "assistant", content: "Model-backed reply." } }] }) {
    const calls: { path: string; headers: IncomingHttpHeaders; body: Record<string, unknown> }[] = [];
    const server = createServer(async (req, res) => {
        const chunks: Buffer[] = [];
        for await (const chunk of req) chunks.push(Buffer.from(chunk));
        calls.push({ path: req.url ?? "", headers: req.headers, body: JSON.parse(Buffer.concat(chunks).toString()) });
        res.writeHead(status, { "Content-Type": "application/json", Location: "http://127.0.0.1:1/never" });
        res.end(JSON.stringify(response));
    });
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    cleanup.push(() => new Promise<void>((resolve, reject) => server.close(err => err ? reject(err) : resolve())));
    return { baseUrl: `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1`, calls };
}

function run(raw: string, baseUrl?: string) {
    return new Promise<{ code: number | null; stdout: string; stderr: string }>((resolve, reject) => {
        const child = spawn(process.execPath, ["dist/agent.js"], {
            env: { ...process.env, MODEL_BASE_URL: baseUrl ?? "", MODEL_NAME: "test-model", OPENAI_API_KEY: "ambient-key-do-not-send", VLLM_API_KEY: "also-do-not-send" },
            stdio: ["pipe", "pipe", "pipe"],
        });
        let stdout = "";
        let stderr = "";
        child.stdout.on("data", chunk => { stdout += String(chunk); });
        child.stderr.on("data", chunk => { stderr += String(chunk); });
        child.once("error", reject);
        child.once("close", code => resolve({ code, stdout, stderr }));
        child.stdin.end(raw);
    });
}

describe("launcher → unchanged shared runAgent → mock model endpoint", () => {
    it.each([
        { session_id: "buzz-thread", message_text: "hello" },
        { session_id: "dimension-session", content: [{ type: "text", text: "hello" }] },
    ])("prints only final model text for %j", async payload => {
        const mock = await mockEndpoint();
        const result = await run(JSON.stringify({ ...payload, history: [{ role: "user", content: "do not send history" }] }), mock.baseUrl);
        expect(result).toEqual({ code: 0, stdout: "Model-backed reply.", stderr: "" });
        expect(mock.calls).toHaveLength(1);
        expect(mock.calls[0].path).toBe("/v1/chat/completions");
        expect(mock.calls[0].headers.authorization).toBeUndefined();
        expect(mock.calls[0].body).toMatchObject({ model: "test-model", stream: false, max_tokens: 2048, messages: [
            { role: "system", content: expect.stringContaining("no tools") }, { role: "user", content: "hello" },
        ] });
        expect(mock.calls[0].body).not.toHaveProperty("tools");
        expect(JSON.stringify(mock.calls[0])).not.toContain("do-not-send");
    });
    it.each([401, 500, 302])("fails without stdout for HTTP %s (including redirects)", async status => {
        const mock = await mockEndpoint(status);
        const result = await run('{"message_text":"hello"}', mock.baseUrl);
        expect(result.code).toBe(1);
        expect(result.stdout).toBe("");
        expect(result.stderr).toContain("Model produced no final text reply");
        expect(mock.calls).toHaveLength(1);
    });
    it.each([
        {},
        { choices: [{ finish_reason: "stop", message: { role: "assistant", content: "" } }] },
        { choices: [{ finish_reason: "length", message: { role: "assistant", content: "partial" } }] },
        { choices: [{ finish_reason: "tool_calls", message: { role: "assistant", content: "tool request", tool_calls: [{}] } }] },
    ])("fails closed on invalid or unfinished model response %j", async response => {
        const mock = await mockEndpoint(200, response);
        const result = await run('{"message_text":"hello"}', mock.baseUrl);
        expect(result.code).toBe(1);
        expect(result.stdout).toBe("");
        expect(result.stderr).toContain("complete text assistant reply");
    });
    it("fails before network access when configuration is missing", async () => {
        const result = await run('{"message_text":"hello"}');
        expect(result.code).toBe(1);
        expect(result.stdout).toBe("");
        expect(result.stderr).toContain("MODEL_BASE_URL and MODEL_NAME are required");
    });
    it("rejects malformed bridge payload before contacting the endpoint", async () => {
        const mock = await mockEndpoint();
        const result = await run('{"message_text":123}', mock.baseUrl);
        expect(result.code).toBe(1);
        expect(result.stdout).toBe("");
        expect(mock.calls).toHaveLength(0);
    });
});
