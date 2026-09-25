"""Cognee-Rust HTTP load tests using Locust.

This is a port of the Python SDK Locust benchmark tailored for the Rust HTTP
server.

Default mode is Model A (authorization disabled):
- benchmark runner starts server with REQUIRE_AUTHENTICATION=false
- requests are sent without X-Api-Key

If you provide COGNEE_API_KEY, the header is sent.
"""

from __future__ import annotations

import csv
import io
import os
import random
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from datetime import datetime
from pathlib import Path

from locust import HttpUser, SequentialTaskSet, between, events, tag, task

API_KEY = os.environ.get("COGNEE_API_KEY", "")
SEARCH_TYPE = os.environ.get("COGNEE_SEARCH_TYPE", "GRAPH_COMPLETION")
RUST_SERVER_BIN = os.environ.get("COGNEE_HTTP_SERVER_BIN", "cognee-http-server")

ENDPOINT_NAMES = ("/api/v1/add", "/api/v1/cognify", "/api/v1/search")


TOPICS = [
    "quantum computing",
    "machine learning",
    "climate change",
    "renewable energy",
    "space exploration",
    "genetic engineering",
    "blockchain technology",
    "artificial intelligence",
    "ocean conservation",
    "urban planning",
    "medieval history",
    "philosophy of mind",
    "distributed systems",
    "neuroscience",
    "economic theory",
]

SUBTOPICS = [
    "data analysis",
    "pattern recognition",
    "resource allocation",
    "risk assessment",
    "optimization algorithms",
    "predictive modeling",
    "system integration",
    "scalability",
    "error correction",
    "signal processing",
    "network topology",
    "feedback loops",
    "energy efficiency",
    "material science",
    "behavioral adaptation",
    "information theory",
]

SENTENCE_TEMPLATES = [
    "The field of {topic} has seen remarkable advances in recent years, particularly in the area of {subtopic}.",
    "Researchers studying {topic} have discovered that {subtopic} plays a crucial role in understanding the broader implications.",
    "A comprehensive review of {topic} literature reveals that {subtopic} remains one of the most debated aspects.",
    "Recent experiments in {topic} demonstrate a strong correlation between {subtopic} and observed outcomes.",
    "The intersection of {topic} and {subtopic} opens new possibilities for practical applications.",
    "Experts in {topic} argue that {subtopic} will be the defining challenge of the next decade.",
    "Historical analysis shows that {topic} has always been influenced by developments in {subtopic}.",
    "New computational models for {topic} suggest that {subtopic} can be optimized through iterative approaches.",
    "The economic impact of {topic} is closely tied to advancements in {subtopic}, according to recent studies.",
    "Collaborative efforts in {topic} have led to breakthroughs in {subtopic} that were previously thought impossible.",
    "Understanding {topic} requires a deep appreciation of how {subtopic} interacts with existing frameworks.",
    "Policy makers are increasingly turning to {topic} research to inform decisions about {subtopic}.",
]

SEARCH_QUERIES = [
    "What are the main findings about {topic}?",
    "How does {topic} relate to recent developments?",
    "What are the key challenges in {topic}?",
    "Summarize the information about {topic}.",
    "What practical applications exist for {topic}?",
    "What is the current state of research in {topic}?",
]


def parse_bool(value: str | None, default: bool = False) -> bool:
    if value is None:
        return default
    return value.strip().lower() in {"1", "true", "yes", "on"}


def read_endpoint_averages(stats_csv_path: Path) -> dict[str, float]:
    averages: dict[str, float] = {}
    with stats_csv_path.open(newline="", encoding="utf-8") as f:
        for row in csv.DictReader(f):
            if row["Name"] in ENDPOINT_NAMES:
                averages[row["Name"]] = float(row["Average Response Time"])
    return averages


def generate_paragraph(topic: str, num_sentences: int = 5) -> str:
    sentences = []
    for _ in range(num_sentences):
        template = random.choice(SENTENCE_TEMPLATES)
        subtopic = random.choice(SUBTOPICS)
        sentences.append(template.format(topic=topic, subtopic=subtopic))
    return " ".join(sentences)


def generate_document(num_paragraphs: int = 3) -> tuple[str, str]:
    topic = random.choice(TOPICS)
    paragraphs = [
        generate_paragraph(topic, num_sentences=random.randint(300, 377))
        for _ in range(num_paragraphs)
    ]
    paragraphs.append(str(uuid.uuid4()))
    return "\n\n".join(paragraphs), topic


def generate_search_query(topic: str) -> str:
    template = random.choice(SEARCH_QUERIES)
    return template.format(topic=topic)


def maybe_headers(api_key: str, json_content_type: bool = False) -> dict[str, str]:
    headers: dict[str, str] = {}
    if api_key:
        headers["X-Api-Key"] = api_key
    if json_content_type:
        headers["Content-Type"] = "application/json"
    return headers


@events.init_command_line_parser.add_listener
def add_custom_arguments(parser):
    parser.add_argument(
        "--cognee-api-key",
        type=str,
        default="",
        help="API key for Cognee (optional in no-auth mode)",
    )


@events.test_start.add_listener
def on_test_start(environment, **kwargs):
    require_api_key = parse_bool(os.environ.get("COGNEE_REQUIRE_API_KEY"), False)
    api_key = environment.parsed_options.cognee_api_key or API_KEY
    if require_api_key and not api_key:
        environment.runner.quit()
        raise SystemExit(
            "COGNEE_REQUIRE_API_KEY=true but no key provided. Set COGNEE_API_KEY or pass --cognee-api-key."
        )


class AddCognifySearchFlow(SequentialTaskSet):
    dataset_name: str = ""
    api_key: str = ""
    topic: str = ""

    @tag("add")
    @task
    def add_text(self):
        text, self.topic = generate_document(num_paragraphs=random.randint(2, 5))
        form_data = {"datasetName": self.dataset_name}
        files = [("data", ("document.txt", io.BytesIO(text.encode("utf-8")), "text/plain"))]

        with self.client.post(
            "/api/v1/add",
            data=form_data,
            files=files,
            headers=maybe_headers(self.api_key),
            name="/api/v1/add",
            catch_response=True,
            timeout=600,
        ) as resp:
            if resp.status_code == 200:
                resp.success()
            else:
                resp.failure(f"Add failed: {resp.status_code} - {resp.text[:300]}")

    @tag("cognify")
    @task
    def cognify(self):
        payload = {"datasets": [self.dataset_name], "runInBackground": False}
        with self.client.post(
            "/api/v1/cognify",
            json=payload,
            headers=maybe_headers(self.api_key, json_content_type=True),
            name="/api/v1/cognify",
            catch_response=True,
            timeout=6000,
        ) as resp:
            if resp.status_code == 200:
                resp.success()
            else:
                resp.failure(f"Cognify failed: {resp.status_code} - {resp.text[:300]}")

    @tag("search")
    @task
    def search(self):
        query = generate_search_query(self.topic or random.choice(TOPICS))
        payload = {
            "searchType": SEARCH_TYPE,
            "query": query,
            "datasets": [self.dataset_name],
            "only_context": True,
        }
        with self.client.post(
            "/api/v1/search",
            json=payload,
            headers=maybe_headers(self.api_key, json_content_type=True),
            name="/api/v1/search",
            catch_response=True,
            timeout=200,
        ) as resp:
            if resp.status_code == 200:
                resp.success()
            else:
                resp.failure(f"Search failed: {resp.status_code} - {resp.text[:300]}")
        self.interrupt()


class MultiDatasetFlow(AddCognifySearchFlow):
    """Each user call uses a unique dataset."""

    def on_start(self):
        uid = uuid.uuid4().hex[:8]
        self.dataset_name = f"loadtest_user_{uid}"
        self.api_key = self.user.environment.parsed_options.cognee_api_key or API_KEY


class MultiDatasetCogneeTest(HttpUser):
    tasks = [MultiDatasetFlow]
    wait_time = between(5, 10)
    weight = 2


SHARED_DATASET_NAME = f"loadtest_shared_{uuid.uuid4().hex[:8]}"


class SingleDatasetFlow(AddCognifySearchFlow):
    """All virtual users share the same dataset."""

    def on_start(self):
        self.dataset_name = SHARED_DATASET_NAME
        self.api_key = self.user.environment.parsed_options.cognee_api_key or API_KEY


class SingleDatasetCogneeTest(HttpUser):
    tasks = [SingleDatasetFlow]
    wait_time = between(5, 10)
    # TODO: Return weight when scenario is fixed to handle concurrent cognify calls on the same dataset.
    weight = 0


def wait_for_server(url: str, timeout: float = 240.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2) as resp:
                if resp.status == 200:
                    return
        except (urllib.error.URLError, ConnectionError):
            pass
        time.sleep(0.5)
    raise SystemExit(f"Cognee server at {url} did not become ready in {timeout}s")


def prepare_server_environment(manage_server: bool) -> tuple[dict[str, str], Path | None]:
    """Prepare environment for Rust server startup.

    When benchmark-managed server mode is enabled, this defaults to isolated
    state roots per run to avoid cross-run contamination from previous Ladybug
    or sqlite/vector state.
    """
    env = {
        **os.environ,
        "REQUIRE_AUTHENTICATION": "false",
    }

    if not manage_server:
        return env, None

    use_unique_state = parse_bool(os.environ.get("COGNEE_LOCUST_UNIQUE_STATE"), True)
    if not use_unique_state:
        return env, None

    keep_state = parse_bool(os.environ.get("COGNEE_LOCUST_KEEP_STATE"), False)
    configured_root = os.environ.get("COGNEE_LOCUST_STATE_ROOT", "").strip()

    cleanup_root: Path | None = None
    if configured_root:
        state_root = Path(configured_root).expanduser().resolve()
        if state_root.exists():
            shutil.rmtree(state_root)
        state_root.mkdir(parents=True, exist_ok=True)
    else:
        state_root = Path(tempfile.mkdtemp(prefix="cognee-locust-state-"))
        if not keep_state:
            cleanup_root = state_root

    data_root = state_root / "data"
    system_root = state_root / "system"
    session_root = state_root / "sessions"
    data_root.mkdir(parents=True, exist_ok=True)
    system_root.mkdir(parents=True, exist_ok=True)
    session_root.mkdir(parents=True, exist_ok=True)

    env.update(
        {
            "DATA_ROOT_DIRECTORY": str(data_root),
            "SYSTEM_ROOT_DIRECTORY": str(system_root),
            "COGNEE_SESSION_DIR": str(session_root),
        }
    )

    if cleanup_root is None:
        print(f"Using benchmark server state root: {state_root}")
    else:
        print(f"Using isolated benchmark server state root: {state_root}")

    return env, cleanup_root


def start_rust_server(host: str, port: str, server_env: dict[str, str]) -> subprocess.Popen:
    env = {
        **server_env,
        "HTTP_API_HOST": host,
        "HTTP_API_PORT": port,
    }

    # Prefer an explicit binary path if set; otherwise use cargo run.
    if RUST_SERVER_BIN and Path(RUST_SERVER_BIN).exists():
        cmd = [RUST_SERVER_BIN, "--host", host, "--port", port]
    else:
        cmd = [
            "cargo",
            "run",
            "-p",
            "cognee-http-server",
            "--features",
            "bin",
            "--bin",
            "cognee-http-server",
            "--",
            "--host",
            host,
            "--port",
            port,
        ]

    return subprocess.Popen(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
        env=env,
    )


def run_headless(base_url: str, extra_args: list[str]) -> int:
    now = datetime.now()
    timestamp = now.strftime("%Y%m%d_%H%M%S")
    result_folder = Path("results")
    result_folder.mkdir(exist_ok=True)
    result_location = result_folder / f"locust_run_{timestamp}"
    html_result_location = f"{result_location}.html"
    locust_log_location = Path(f"{result_location}.log")
    locust_log_location.touch()

    cmd = [
        sys.executable,
        "-m",
        "locust",
        "-f",
        __file__,
        "--host",
        base_url,
        "--csv",
        str(result_location),
        "--html",
        html_result_location,
        "--logfile",
        str(locust_log_location),
        "--headless",
        "-u",
        os.environ.get("COGNEE_LOCUST_USERS", "10"),
        "-r",
        os.environ.get("COGNEE_LOCUST_SPAWN_RATE", "1"),
        "--run-time",
        os.environ.get("COGNEE_LOCUST_RUN_TIME", "5m"),
        "MultiDatasetCogneeTest",
        *extra_args,
    ]

    rc = subprocess.run(cmd, env={**os.environ}).returncode

    stats_csv = Path(f"{result_location}_stats.csv")
    if stats_csv.exists():
        averages = read_endpoint_averages(stats_csv)
        print("\n=== Average response times (ms) ===")
        for name in ENDPOINT_NAMES:
            avg = averages.get(name)
            if avg is None:
                print(f"  {name}: (no requests recorded)")
            else:
                print(f"  {name}: {avg:.0f} ms")
    else:
        print(f"\nNo stats CSV found at {stats_csv}; skipping averages.")

    print(f"\nResults base path: {result_location}")
    return rc


if __name__ == "__main__":
    host = os.environ.get("HTTP_API_HOST", "127.0.0.1")
    port = os.environ.get("HTTP_API_PORT", "8000")
    base_url = f"http://{host}:{port}"

    manage_server = parse_bool(os.environ.get("COGNEE_LOCUST_MANAGE_SERVER"), True)

    server_proc: subprocess.Popen | None = None
    cleanup_state_root: Path | None = None
    try:
        server_env, cleanup_state_root = prepare_server_environment(manage_server)
        if manage_server:
            server_proc = start_rust_server(host, port, server_env)
        wait_for_server(f"{base_url}/health")
        exit_code = run_headless(base_url, sys.argv[1:])
    finally:
        if server_proc is not None:
            try:
                os.killpg(server_proc.pid, signal.SIGTERM)
                server_proc.wait(timeout=15)
            except ProcessLookupError:
                pass
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(server_proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if cleanup_state_root is not None:
            shutil.rmtree(cleanup_state_root, ignore_errors=True)

    sys.exit(exit_code)
