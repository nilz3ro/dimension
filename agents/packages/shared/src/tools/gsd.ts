import { exec } from "node:child_process";
import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import { AgentTool } from "@mariozechner/pi-agent-core";

const GSD_BIN = process.env["GSD_BIN"] ?? "node /app/gsd/bin/gsd-tools.cjs";

const GsdInput = Type.Object({
    command: Type.String({ description: `GSD tools command. Available commands:
- state load — Load project config + state
- state json — STATE.md frontmatter as JSON
- state update <field> <value> — Update STATE.md field
- state get [section] — Get STATE.md content
- resolve-model <agent-type> — Get model for agent
- find-phase <phase> — Find phase directory
- commit <message> --files f1 f2 — Commit planning docs
- verify-summary <path> — Verify SUMMARY.md
- generate-slug <text> — URL-safe slug
- current-timestamp [format] — Timestamp
- list-todos [area] — Count pending todos
- verify-path-exists <path> — Check existence
- history-digest — Aggregate SUMMARY data
- phase next-decimal <phase> — Next decimal phase number
- phase add <description> — Add phase to roadmap
- phase insert <after> <description> — Insert decimal phase
- phase remove <phase> — Remove phase
- phase complete <phase> — Mark phase done
- roadmap get-phase <phase> — Extract phase section
- roadmap analyze — Full roadmap parse
- frontmatter validate <path> --schema <type> — Validate frontmatter
- verify plan-structure <path> — Validate plan structure
- template <name> — Get template content
- init <workflow> <phase> — Initialize workflow context` }),
    cwd: Type.Optional(Type.String({ description: "Working directory (default: /app/workspace)" })),
});

export const gsdTool: AgentTool = {
    name: "gsd",
    label: "GSD Tools",
    description: "Run GSD (Get Shit Done) workflow commands for project planning, state management, phase operations, and roadmap management. Use for structured planning workflows.",
    parameters: GsdInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(GsdInput, params);
        const cwd = p.cwd ?? process.env["WORKSPACE_DIR"] ?? "/app/workspace";
        const cmd = `${GSD_BIN} ${p.command}`;

        return new Promise((resolve) => {
            exec(cmd, { cwd, timeout: 30000, maxBuffer: 512 * 1024 }, (error, stdout, stderr) => {
                const output: string[] = [];
                if (stdout) output.push(stdout);
                if (stderr) output.push(stderr);
                if (error) output.push(`Exit code: ${error.code}`);

                resolve({
                    content: [{ type: "text", text: output.join("\n") || "(no output)" }],
                    details: { exitCode: error?.code ?? 0 },
                });
            });
        });
    },
};
