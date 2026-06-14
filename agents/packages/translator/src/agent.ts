/**
 * Translator agent — uses vLLM for translation.
 * No tools — pure chat completion via OpenAI-compatible API.
 * Standalone: reads stdin JSON, writes response to stdout and emits the
 * final message via dimension.sendMessage (UDS → vsock → SSE).
 */

import fs from "node:fs";
import { sendMessage } from "@dimension-agents/shared";

const SYSTEM_PROMPT = fs.readFileSync(
    new URL("../src/prompts/translator.md", import.meta.url),
    "utf-8"
);

const VLLM_BASE = process.env.VLLM_BASE_URL || "http://100.104.71.102:8000";
const MODEL = process.env.VLLM_MODEL || "openai/gpt-oss-20b";

interface DimensionContent { type: string; text?: string; }
interface HistoryEntry { role: string; content: string; timestamp: string; }
interface DimensionRequest {
    role: string;
    content: DimensionContent[];
    session_id: string;
    bundle_id: string;
    history: HistoryEntry[];
}
interface ChatMessage { role: "system" | "user" | "assistant"; content: string; }

function readStdin(): Promise<string> {
    return new Promise((resolve, reject) => {
        const chunks: Buffer[] = [];
        process.stdin.on("data", (chunk: Buffer) => chunks.push(chunk));
        process.stdin.on("end", () => resolve(Buffer.concat(chunks).toString("utf-8")));
        process.stdin.on("error", reject);
    });
}

async function emit(content: string, sessionId: string): Promise<void> {
    process.stdout.write(content);
    await sendMessage(content, { sessionId });
}

async function main() {
    const raw = await readStdin();
    const request: DimensionRequest = JSON.parse(raw);

    const userText = request.content
        .filter(b => b.type === "text" && b.text)
        .map(b => b.text!)
        .join("\n");

    if (!userText) {
        await emit("No text provided.", request.session_id);
        return;
    }

    // Build messages: system + history + user
    const messages: ChatMessage[] = [{ role: "system", content: SYSTEM_PROMPT }];
    if (request.history) {
        for (const entry of request.history) {
            messages.push({
                role: entry.role === "user" ? "user" : "assistant",
                content: entry.content,
            });
        }
    }
    messages.push({ role: "user", content: userText });

    const resp = await fetch(`${VLLM_BASE}/v1/chat/completions`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model: MODEL, messages, stream: false }),
    });

    if (!resp.ok) {
        const body = await resp.text();
        process.stderr.write(`vLLM error (${resp.status}): ${body}\n`);
        await emit(`Translation failed: ${resp.status}`, request.session_id);
        return;
    }

    const data = await resp.json() as any;
    const content = data.choices?.[0]?.message?.content || "No translation returned.";
    await emit(content, request.session_id);
}

main().catch((err: unknown) => {
    process.stderr.write(String(err) + "\n");
    process.exit(1);
});
