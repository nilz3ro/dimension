FROM node:22-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

RUN printf "nameserver 8.8.8.8\nnameserver 1.1.1.1\n" > /etc/resolv.conf.vm

RUN corepack enable && corepack prepare pnpm@latest --activate

WORKDIR /app

# Copy workspace root
COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY packages/shared/package.json packages/shared/
COPY packages/artifact-agent/package.json packages/artifact-agent/

RUN pnpm install --frozen-lockfile

COPY packages/shared/ packages/shared/
COPY packages/artifact-agent/ packages/artifact-agent/

RUN pnpm -r build

COPY packages/artifact-agent/entrypoint.sh /app/entrypoint.sh
COPY packages/artifact-agent/dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

RUN mkdir -p /app/workspace
ENV WORKSPACE_DIR=/app/workspace
ENV VLLM_BASE_URL=http://100.104.71.102:8000
ENV VLLM_MODEL=openai/gpt-oss-20b
ENV VLLM_API_KEY=none

ENTRYPOINT ["/app/entrypoint.sh"]
