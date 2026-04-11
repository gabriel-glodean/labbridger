FROM python:3.12-slim
RUN pip install --no-cache-dir flask
WORKDIR /app
COPY mock_ollama.py .
EXPOSE 11434
CMD ["python", "-u", "mock_ollama.py"]
