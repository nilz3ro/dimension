import fs from "node:fs/promises";
import path from "node:path";
import { Type } from "@sinclair/typebox";
import { Value } from "@sinclair/typebox/value";
import { AgentTool } from "@mariozechner/pi-agent-core";

const ReadFileInput = Type.Object({
    path: Type.String({ description: "Absolute or relative file path to read" }),
});

export const readFileTool: AgentTool = {
    name: "read_file",
    label: "Read File",
    description: "Read the contents of a file. Returns the full text content.",
    parameters: ReadFileInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(ReadFileInput, params);
        const content = await fs.readFile(p.path, "utf-8");
        return {
            content: [{ type: "text", text: content }],
            details: { path: p.path, size: content.length },
        };
    },
};

const WriteFileInput = Type.Object({
    path: Type.String({ description: "File path to write to" }),
    content: Type.String({ description: "Content to write" }),
});

export const writeFileTool: AgentTool = {
    name: "write_file",
    label: "Write File",
    description: "Write content to a file. Creates parent directories if needed. Overwrites existing files.",
    parameters: WriteFileInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(WriteFileInput, params);
        await fs.mkdir(path.dirname(p.path), { recursive: true });
        await fs.writeFile(p.path, p.content, "utf-8");
        return {
            content: [{ type: "text", text: `Written ${p.content.length} bytes to ${p.path}` }],
            details: { path: p.path, size: p.content.length },
        };
    },
};

const EditFileInput = Type.Object({
    path: Type.String({ description: "File path to edit" }),
    old_text: Type.String({ description: "Exact text to find (must match exactly including whitespace)" }),
    new_text: Type.String({ description: "Replacement text" }),
});

export const editFileTool: AgentTool = {
    name: "edit_file",
    label: "Edit File",
    description: "Edit a file by replacing exact text. The old_text must match exactly (including whitespace and newlines). Use this for precise, surgical edits.",
    parameters: EditFileInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(EditFileInput, params);
        const content = await fs.readFile(p.path, "utf-8");
        if (!content.includes(p.old_text)) {
            return {
                content: [{ type: "text", text: `Error: Could not find the exact text to replace in ${p.path}. The old_text must match exactly.` }],
                details: { error: true },
            };
        }
        const newContent = content.replace(p.old_text, p.new_text);
        await fs.writeFile(p.path, newContent, "utf-8");
        return {
            content: [{ type: "text", text: `Edited ${p.path}` }],
            details: { path: p.path },
        };
    },
};

const ListDirInput = Type.Object({
    path: Type.String({ description: "Directory path to list" }),
    recursive: Type.Optional(Type.Boolean({ description: "List recursively (default: false)" })),
});

async function listRecursive(dir: string, maxDepth: number = 5, depth: number = 0): Promise<string[]> {
    if (depth >= maxDepth) return [];
    const entries = await fs.readdir(dir, { withFileTypes: true });
    const results: string[] = [];
    for (const entry of entries) {
        if (entry.name === "node_modules" || entry.name === ".git" || entry.name === "dist") continue;
        const fullPath = path.join(dir, entry.name);
        if (entry.isDirectory()) {
            results.push(fullPath + "/");
            const sub = await listRecursive(fullPath, maxDepth, depth + 1);
            results.push(...sub);
        } else {
            results.push(fullPath);
        }
    }
    return results;
}

export const listDirTool: AgentTool = {
    name: "list_directory",
    label: "List Directory",
    description: "List files and directories. Skips node_modules, .git, dist. Set recursive=true to list all files in the tree.",
    parameters: ListDirInput,
    execute: async (_toolCallId, params, _signal, _onUpdate) => {
        const p = Value.Parse(ListDirInput, params);
        let files: string[];
        if (p.recursive) {
            files = await listRecursive(p.path);
        } else {
            const entries = await fs.readdir(p.path, { withFileTypes: true });
            files = entries.map(e => e.name + (e.isDirectory() ? "/" : ""));
        }
        return {
            content: [{ type: "text", text: files.join("\n") }],
            details: { count: files.length },
        };
    },
};
