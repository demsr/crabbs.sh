FROM node:latest
RUN apt-get update && apt-get install -y --no-install-recommends \
    chromium sqlite3 fonts-liberation curl build-essential \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /home/node/.claude && chown -R node:node /home/node/.claude
USER node
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/home/node/.cargo/bin:${PATH}"