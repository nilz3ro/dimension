import { exec } from "node:child_process";
import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import { AgentTool } from "@mariozechner/pi-agent-core";

const GrepInput = Type.Object({
    pattern: Type.String({ description: "Search pattern (regex or literal string)" }),
    path: Type.String({ description: "File or directory to search in" }),
    include: Type.Optional(Type.String({ description: "File glob pattern to include (e.g. '*.ts', '*.rs')" })),
    context_lines: Type.Optional(Type.Number({ description: "Number of context lines around matches (default: 2)" })),
});

export const grepTool: AgentTool = {
    name: "grep",
    label: "Search Code",
    description: "Search for a pattern in files using grep. Returns matching lines with context. Use to find function definitions, usages, imports, etc.",
    parameters: GrepInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(GrepInput, params);
        const ctx = p.context_lines ?? 2;
        const includeFlag = p.include ? `--include='${p.include}'` : "";
        const cmd = `grep -rn -C ${ctx} ${includeFlag} --color=never '${p.pattern.replace(/'/g, "'\\''")}' ${p.path} 2>&1 | head -200`;

        return new Promise((resolve) => {
            exec(cmd, { timeout: 10000, maxBuffer: 512 * 1024 }, (_error, stdout) => {
                resolve({
                    content: [{ type: "text", text: stdout || "No matches found." }],
                    details: {},
                });
            });
        });
    },
};
