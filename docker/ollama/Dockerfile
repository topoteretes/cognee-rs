# Custom Ollama Dockerfile with pre-loaded model
FROM ollama/ollama:latest

# Set environment variables
ENV OLLAMA_HOST=0.0.0.0:11434

# Copy initialization script
COPY init-model.sh /init-model.sh
RUN chmod +x /init-model.sh

# Expose Ollama API port
EXPOSE 11434

# Use custom entrypoint to pull model on first start
ENTRYPOINT ["/init-model.sh"]
