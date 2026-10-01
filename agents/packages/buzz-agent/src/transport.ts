import { createAssistantMessageEventStream, type AssistantMessage, type StreamFunction } from "@mariozechner/pi-ai";

/** One text-only, non-streaming OpenAI-compatible completion; no auth or tools. */
export const chatCompletion: StreamFunction = (model, context, options) => {
    const events = createAssistantMessageEventStream();
    const output: AssistantMessage = {
        role: "assistant",
        content: [],
        api: model.api,
        provider: model.provider,
        model: model.id,
        usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
            cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
        stopReason: "stop",
        timestamp: Date.now(),
    };
    void (async () => {
        try {
            if (context.tools?.length) throw new Error("buzz-agent does not support tools");
            const messages: { role: string; content: string }[] = [];
            if (context.systemPrompt) messages.push({ role: "system", content: context.systemPrompt });
            for (const message of context.messages) {
                if (message.role !== "user" && message.role !== "assistant") {
                    throw new Error("buzz-agent accepts only user and assistant messages");
                }
                const content = typeof message.content === "string" ? message.content : message.content.map(block => {
                    if (block.type !== "text") throw new Error("buzz-agent accepts only text");
                    return block.text;
                }).join("\n");
                messages.push({ role: message.role, content });
            }
            const response = await fetch(`${model.baseUrl}/chat/completions`, {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                // Fail rather than follow a redirect to a different model endpoint.
                redirect: "error",
                signal: options?.signal ? AbortSignal.any([options.signal, AbortSignal.timeout(210_000)]) : AbortSignal.timeout(210_000),
                body: JSON.stringify({ model: model.id, messages, stream: false, max_tokens: model.maxTokens }),
            });
            if (!response.ok) throw new Error(`Model endpoint returned HTTP ${response.status}`);
            const data = await response.json() as {
                choices?: { finish_reason?: string; message?: { role?: string; content?: unknown; tool_calls?: unknown[] } }[];
            };
            const choice = data?.choices?.[0];
            const message = choice?.message;
            if (!message || message.role !== "assistant" || message.tool_calls?.length ||
                typeof message.content !== "string" || !message.content.trim() || choice?.finish_reason !== "stop") {
                throw new Error("Model endpoint did not return a complete text assistant reply");
            }
            output.content = [{ type: "text", text: message.content }];
            events.push({ type: "done", reason: "stop", message: output });
        } catch (err: unknown) {
            output.stopReason = "error";
            output.errorMessage = String(err);
            // Shared runAgent emits only text, not provider errors. Surface the
            // cause on stderr; the launcher makes empty stdout a failed turn.
            process.stderr.write(`${output.errorMessage}\n`);
            events.push({ type: "error", reason: "error", error: output });
        }
    })();
    return events;
};
