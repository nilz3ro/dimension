import { spawn } from "node:child_process";
import { normalizePayload } from "./input.js";

async function main(): Promise<void> {
    const chunks: Buffer[] = [];
    for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
    const input = normalizePayload(Buffer.concat(chunks).toString("utf-8"));
    const request = JSON.parse(input) as { session_id: string };
    // Pipe normalized stdin to the unchanged shared harness in a child process.
    const child = spawn(process.execPath, [new URL("./runtime.js", import.meta.url).pathname], {
        stdio: ["pipe", "pipe", "inherit"],
        env: { ...process.env, CONVERSATION_ID: request.session_id },
    });
    const forwardTerm = (): void => { child.kill("SIGTERM"); };
    const forwardInt = (): void => { child.kill("SIGINT"); };
    process.on("SIGTERM", forwardTerm);
    process.on("SIGINT", forwardInt);
    const output: Buffer[] = [];
    child.stdout.on("data", (chunk: Buffer) => output.push(chunk));
    child.stdin.on("error", (err: NodeJS.ErrnoException) => {
        if (err.code !== "EPIPE") process.stderr.write(`Input pipe failed: ${err.message}\n`);
    });
    child.stdin.end(input);
    const code = await new Promise<number>((resolve, reject) => {
        child.once("error", reject);
        child.once("close", (status, signal) => resolve(status ?? (signal === "SIGINT" ? 130 : 143)));
    });
    process.off("SIGTERM", forwardTerm);
    process.off("SIGINT", forwardInt);
    if (code !== 0) {
        process.exitCode = code;
        return;
    }
    const text = Buffer.concat(output).toString("utf-8");
    // The shared harness can exit zero after a provider error without a reply.
    // Do not let that appear as a successful model-backed turn to the bridge.
    if (!text.trim()) throw new Error("Model produced no final text reply");
    process.stdout.write(text);
}

main().catch((err: unknown) => {
    process.stderr.write(String(err) + "\n");
    process.exitCode = 1;
});
