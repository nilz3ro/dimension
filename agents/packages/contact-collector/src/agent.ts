/**
 * contact-collector — Dimension agent that collects contact information.
 *
 * Uses an LLM to extract first name, last name, email, and phone number
 * from natural language input. Returns a JSON object with those fields.
 */

import fs from "node:fs";
import { runAgent } from "@dimension-agents/shared";

const prompt = fs.readFileSync(
    new URL("../src/prompts/contact-collector.md", import.meta.url),
    "utf-8"
);

runAgent({
    systemPrompt: prompt,
    tools: [],
}).catch((err: unknown) => {
    process.stderr.write(String(err) + "\n");
    process.exit(1);
});
