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
COPY packages/translator/package.json packages/translator/

RUN pnpm install --frozen-lockfile

COPY packages/shared/ packages/shared/
COPY packages/translator/ packages/translator/

RUN pnpm -r build

COPY packages/translator/entrypoint.sh /app/entrypoint.sh
COPY packages/translator/dimension.toml /etc/hyphae/dimension.toml
RUN chmod +x /app/entrypoint.sh

ENV VLLM_BASE_URL=http://100.104.71.102:8000
ENV VLLM_MODEL=openai/gpt-oss-20b

ENTRYPOINT ["/app/entrypoint.sh"]
