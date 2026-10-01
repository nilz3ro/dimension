import { runAgent } from "@dimension-agents/shared";
import { createChatModel } from "./model.js";

try {
    await runAgent({
        systemPrompt: "You are DimensionBridge, a helpful assistant in Buzz chat. Answer the user's message directly and concisely. You have no tools and cannot inspect files, execute commands, or verify live systems. Be honest about uncertainty. Each turn is independent; do not claim to remember previous turns.",
        tools: [],
        model: createChatModel(),
    });
} catch (err: unknown) {
    process.stderr.write(String(err) + "\n");
    process.exitCode = 1;
}
