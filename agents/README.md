# Dimension Agents

Monorepo for Dimension's built-in agents. Each agent is a package that runs inside Firecracker microVMs.

## Structure

```
packages/
  shared/     Shared tools, agent harness, and utilities
  coder/      Coding agent (read, write, edit, bash, web, a2a)
```

## Shared Tools

All agents can use tools from `@dimension-agents/shared`:

| Tool | Description |
|------|-------------|
| `read_file` | Read file contents |
| `write_file` | Create/overwrite files |
| `edit_file` | Surgical find-and-replace |
| `list_directory` | List files (recursive) |
| `bash` | Shell execution |
| `grep` | Regex code search |
| `web_search` | Firecrawl web search |
| `web_fetch` | Fetch + extract URL content |
| `a2a_send` | Send message to A2A agent |
| `a2a_discover` | Fetch agent card |

## Adding a New Agent

1. Create `packages/my-agent/`
2. Add `package.json` with `@dimension-agents/shared` dependency
3. Write `src/agent.ts` — import `runAgent` + pick your tools
4. Add a system prompt at `src/prompts/`
5. Add `Dockerfile` and `entrypoint.sh`

```typescript
// packages/my-agent/src/agent.ts
import { runAgent, readFileTool, bashTool } from "@dimension-agents/shared";
import fs from "node:fs";

const prompt = fs.readFileSync(new URL("./prompts/my-agent.md", import.meta.url), "utf-8");

runAgent({
  systemPrompt: prompt,
  tools: [readFileTool, bashTool],
});
```
