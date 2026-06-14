import { spawn } from "node:child_process";
import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import { AgentTool } from "@mariozechner/pi-agent-core";

const BashInput = Type.Object({
    command: Type.String({ description: "Bash command or script to execute. Can be multi-line." }),
    cwd: Type.Optional(Type.String({ description: "Working directory (default: /app/workspace)" })),
    timeout_ms: Type.Optional(Type.Number({ description: "Timeout in milliseconds (default: 60000). Long-running builds may need more." })),
});

export const bashTool: AgentTool = {
    name: "bash",
    label: "Bash",
    description: `Execute a bash command or script. Returns combined stdout and stderr.

Use for:
- Running builds: npm run build, cargo build, make
- Running tests: npm test, pytest, cargo test
- Git operations: git status, git diff, git commit
- Installing packages: npm install, pip install, apt-get install
- Inspecting the system: ls, find, cat, head, tail, wc
- Chaining commands: cmd1 && cmd2, cmd1 | cmd2
- Multi-line scripts

The command runs inside a sandboxed Firecracker VM. You have root access.
If a command might produce a lot of output, pipe through head or tail.`,
    parameters: BashInput,
    execute: async (_toolCallId, params, signal, _onUpdate) => {
        const p = Value.Parse(BashInput, params);
        const timeout = p.timeout_ms ?? 60000;
        const cwd = p.cwd ?? process.env["WORKSPACE_DIR"] ?? "/app/workspace";

        return new Promise((resolve) => {
            const chunks: string[] = [];
            let killed = false;

            const child = spawn("bash", ["-c", p.command], {
                cwd,
                stdio: ["pipe", "pipe", "pipe"],
                env: { ...process.env, TERM: "dumb" },
            });

            child.stdout.on("data", (data: Buffer) => chunks.push(data.toString()));
            child.stderr.on("data", (data: Buffer) => chunks.push(data.toString()));

            const timer = setTimeout(() => {
                killed = true;
                child.kill("SIGKILL");
            }, timeout);

            // Respect abort signal from agent
            if (signal) {
                signal.addEventListener("abort", () => {
                    killed = true;
                    child.kill("SIGKILL");
                }, { once: true });
            }

            child.on("close", (code) => {
                clearTimeout(timer);
                let output = chunks.join("");

                // Truncate very long output
                const MAX_OUTPUT = 100_000;
                if (output.length > MAX_OUTPUT) {
                    const half = Math.floor(MAX_OUTPUT / 2);
                    output = output.slice(0, half) + 
                        `\n\n... (${output.length - MAX_OUTPUT} chars truncated) ...\n\n` + 
                        output.slice(-half);
                }

                if (killed) {
                    output += `\n[Process killed after ${timeout}ms timeout]`;
                }

                resolve({
                    content: [{ type: "text", text: output || "(no output)" }],
                    details: { exitCode: code ?? -1, killed, timeout },
                });
            });

            // Close stdin immediately
            child.stdin.end();
        });
    },
};
