"""
Benchmark Datasets — MemoryAgentBench-style test suites

4 Competencies:
1. Accurate Retrieval     — Can we find the exact stored fact?
2. Test-Time Learning     — Can we learn from multi-turn conversations?
3. Long-Range Understanding — Can we connect information across many memories?
4. Conflict Resolution    — Can we handle updates/contradictions in memory?

Each dataset is a list of BenchmarkCase objects containing:
- context_chunks: List of facts/passages to memorize
- queries: List of (query, ground_truth) pairs to test retrieval
"""

from dataclasses import dataclass, field


@dataclass
class BenchmarkCase:
    """A single benchmark case."""
    id: str
    competency: str
    description: str
    context_chunks: list[str]
    queries: list[tuple[str, str]]  # (query, expected_answer)


# =============================================================================
# COMPETENCY 1: ACCURATE RETRIEVAL
# Test: Store facts, then query for specific facts. Can the system find them?
# =============================================================================

ACCURATE_RETRIEVAL = [
    BenchmarkCase(
        id="ar_01",
        competency="accurate_retrieval",
        description="Basic fact retrieval — personal preferences",
        context_chunks=[
            "Alice's favorite color is midnight blue.",
            "Bob prefers to eat sushi for dinner on Fridays.",
            "Charlie's birthday is on March 15th, 1992.",
            "Diana works as a machine learning engineer at Anthropic.",
            "Eve's phone number is 555-0142.",
            "Frank drives a 2019 Tesla Model 3 in pearl white.",
            "Grace speaks four languages: English, Mandarin, French, and Swahili.",
            "Henry's WiFi password is 'sunflower42!'.",
        ],
        queries=[
            ("What is Alice's favorite color?", "midnight blue"),
            ("What does Bob like to eat on Fridays?", "sushi"),
            ("When is Charlie's birthday?", "March 15th, 1992"),
            ("Where does Diana work?", "Anthropic"),
            ("What is Eve's phone number?", "555-0142"),
            ("What kind of car does Frank drive?", "2019 Tesla Model 3"),
            ("How many languages does Grace speak?", "four"),
            ("What is Henry's WiFi password?", "sunflower42!"),
        ],
    ),
    BenchmarkCase(
        id="ar_02",
        competency="accurate_retrieval",
        description="Technical facts — programming and systems",
        context_chunks=[
            "The Rust compiler uses LLVM as its backend for code generation.",
            "SochDB uses HNSW (Hierarchical Navigable Small World) algorithm for approximate nearest neighbor search.",
            "ClawDesk's memory system supports hybrid search combining vector similarity and BM25 keyword matching.",
            "The Tauri framework uses WebView2 on Windows and WKWebView on macOS for rendering.",
            "tokio is Rust's most popular async runtime, using a work-stealing scheduler.",
            "The nomic-embed-text model produces 768-dimensional embeddings.",
            "MVCC (Multi-Version Concurrency Control) allows SochDB to support concurrent reads during writes.",
            "The MemoryManager applies temporal decay with configurable half-life to reduce relevance of old memories.",
        ],
        queries=[
            ("What backend does the Rust compiler use?", "LLVM"),
            ("What algorithm does SochDB use for vector search?", "HNSW"),
            ("What types of search does ClawDesk's memory combine?", "vector similarity and BM25 keyword matching"),
            ("What webview does Tauri use on macOS?", "WKWebView"),
            ("What scheduling strategy does tokio use?", "work-stealing"),
            ("How many dimensions does nomic-embed-text produce?", "768"),
            ("What concurrency technique does SochDB use?", "MVCC"),
            ("What decay function does MemoryManager use?", "temporal decay with configurable half-life"),
        ],
    ),
    BenchmarkCase(
        id="ar_03",
        competency="accurate_retrieval",
        description="Needle in a haystack — find specific fact among many",
        context_chunks=[
            "The average rainfall in Seattle is about 37 inches per year.",
            "Python was created by Guido van Rossum and first released in 1991.",
            "The Great Wall of China is approximately 13,171 miles long.",
            "A group of flamingos is called a 'flamboyance'.",
            "The speed of light is approximately 299,792,458 meters per second.",
            "Mount Everest is 29,031.7 feet (8,848.86 meters) above sea level.",
            "The human body contains approximately 37.2 trillion cells.",
            "Venus rotates in the opposite direction to most planets.",
            "The secret project code name is 'Operation Nighthawk Zero'.",
            "Honey never spoils — archaeologists found 3000-year-old honey in Egyptian tombs.",
            "The smallest country in the world is Vatican City at 0.44 square kilometers.",
            "An octopus has three hearts and blue blood.",
            "The Fibonacci sequence starts: 1, 1, 2, 3, 5, 8, 13, 21, 34, 55.",
            "The chemical formula for table salt is NaCl (sodium chloride).",
            "Black holes were first predicted by Karl Schwarzschild in 1916.",
        ],
        queries=[
            ("What is the secret project code name?", "Operation Nighthawk Zero"),
            ("How long is the Great Wall of China?", "13,171 miles"),
            ("What is a group of flamingos called?", "flamboyance"),
            ("How many hearts does an octopus have?", "three"),
            ("Who first predicted black holes?", "Karl Schwarzschild"),
        ],
    ),
]


# =============================================================================
# COMPETENCY 2: TEST-TIME LEARNING
# Test: Learn from multi-turn conversational context
# =============================================================================

TEST_TIME_LEARNING = [
    BenchmarkCase(
        id="ttl_01",
        competency="test_time_learning",
        description="Multi-turn conversation — user preferences evolving",
        context_chunks=[
            "User: I just started learning Rust last week. Assistant: That's great! Rust is an excellent choice for systems programming.",
            "User: I'm finding ownership and borrowing really confusing. Assistant: That's completely normal. The borrow checker is the hardest concept for most newcomers.",
            "User: I managed to build a simple CLI tool today! Assistant: Congratulations! What did it do?",
            "User: It parses CSV files and generates reports. I used the clap and csv crates. Assistant: Those are solid choices. clap is the standard for CLI argument parsing.",
            "User: Next I want to try building a web API with Axum. Assistant: Axum is excellent for async web services. You'll want to get comfortable with tokio first.",
            "User: Actually, I decided to try Actix-Web instead because my friend recommended it. Assistant: Both are great. Actix-Web has been around longer and has more examples available.",
        ],
        queries=[
            ("When did the user start learning Rust?", "last week"),
            ("What concept does the user find confusing?", "ownership and borrowing"),
            ("What CLI tool did the user build?", "parses CSV files and generates reports"),
            ("What crates did the user use?", "clap and csv"),
            ("Which web framework did the user ultimately choose?", "Actix-Web"),
            ("Why did the user choose that framework?", "friend recommended it"),
        ],
    ),
    BenchmarkCase(
        id="ttl_02",
        competency="test_time_learning",
        description="Task context — debugging session",
        context_chunks=[
            "User: My Tauri app is crashing on startup. The error says 'failed to create webview'. Assistant: This is usually a WebView2 installation issue on Windows.",
            "User: I'm on macOS actually. Assistant: On macOS, WKWebView should always be available. Can you check the Tauri logs for more details?",
            "User: The log says 'NSInternalInconsistencyException'. Assistant: That's a macOS framework error. What version of macOS are you running?",
            "User: macOS 12.6 Monterey. Assistant: Tauri 2.0 requires macOS 10.15 or later, so that should be fine. Let's check if it's a framework conflict.",
            "User: I found it — I had conflicting Sparkle framework versions. Removed the old one and it works now. Assistant: Great! Sparkle conflicts are a known issue with Tauri on macOS.",
        ],
        queries=[
            ("What error was the user seeing?", "failed to create webview"),
            ("What OS is the user on?", "macOS"),
            ("What specific macOS exception occurred?", "NSInternalInconsistencyException"),
            ("What macOS version is the user running?", "macOS 12.6 Monterey"),
            ("What was the root cause of the crash?", "conflicting Sparkle framework versions"),
        ],
    ),
]


# =============================================================================
# COMPETENCY 3: LONG-RANGE UNDERSTANDING
# Test: Connect information stored across multiple independent memories
# =============================================================================

LONG_RANGE_UNDERSTANDING = [
    BenchmarkCase(
        id="lru_01",
        competency="long_range_understanding",
        description="Cross-referencing project details",
        context_chunks=[
            "Project Alpha uses PostgreSQL for its primary database.",
            "The Project Alpha team consists of Sarah (lead), Mike, and Jenny.",
            "Our deployment pipeline uses GitHub Actions for CI/CD.",
            "Sarah mentioned that Project Alpha needs to migrate to MySQL by Q3.",
            "Mike is responsible for writing the database migration scripts.",
            "The staging environment for Project Alpha runs on AWS us-east-1.",
            "Jenny is handling the frontend redesign using React and TypeScript.",
            "The migration deadline was moved from July to September by management.",
            "Mike completed the first batch of migration scripts last Tuesday.",
            "Sarah approved Mike's migration scripts after code review on Wednesday.",
        ],
        queries=[
            ("Who is writing the migration scripts for Project Alpha?", "Mike"),
            ("What database is Project Alpha migrating to?", "MySQL"),
            ("When is the migration deadline?", "September"),
            ("Who approved the migration scripts?", "Sarah"),
            ("What is Jenny working on?", "frontend redesign using React and TypeScript"),
            ("Where does the staging environment run?", "AWS us-east-1"),
        ],
    ),
    BenchmarkCase(
        id="lru_02",
        competency="long_range_understanding",
        description="Personal history reconstruction",
        context_chunks=[
            "My name is Alex and I graduated from MIT in 2018 with a CS degree.",
            "After graduation, I joined Google as a software engineer on the Chrome team.",
            "I left Google in 2020 to start my own company called NeuralPath.",
            "NeuralPath builds AI-powered code review tools.",
            "We raised a $2M seed round from Sequoia in early 2021.",
            "In 2022, we pivoted from code review to AI pair programming.",
            "Our main product is now called CoPilot Pro (not related to GitHub Copilot).",
            "We currently have 15 employees and 200 paying customers.",
            "I'm looking to raise a Series A of $10M in 2024.",
            "My co-founder is Lisa, who I met at Google.",
        ],
        queries=[
            ("Where did Alex go to school?", "MIT"),
            ("What was Alex's first job?", "Google"),
            ("What team was Alex on at Google?", "Chrome team"),
            ("What does NeuralPath currently build?", "AI pair programming"),
            ("How much was the seed round?", "$2M"),
            ("Who funded the seed round?", "Sequoia"),
            ("How did Alex meet Lisa?", "at Google"),
            ("How many paying customers does NeuralPath have?", "200"),
        ],
    ),
]


# =============================================================================
# COMPETENCY 4: CONFLICT RESOLUTION
# Test: Handle contradictory or updated information
# =============================================================================

CONFLICT_RESOLUTION = [
    BenchmarkCase(
        id="cr_01",
        competency="conflict_resolution",
        description="Fact updates — should recall latest version",
        context_chunks=[
            "My favorite programming language is Python.",
            "I work at Microsoft as a software architect.",
            "Actually, I changed jobs last month. I now work at Apple.",
            "My favorite programming language has changed to Rust after using it for a project.",
            "I live in San Francisco.",
            "Update: I just moved to Seattle last week for the new job.",
            "My current project uses React for the frontend.",
            "We decided to switch from React to Svelte for better performance.",
        ],
        queries=[
            ("Where do I work?", "Apple"),
            ("What is my favorite programming language?", "Rust"),
            ("Where do I live?", "Seattle"),
            ("What frontend framework is my project using?", "Svelte"),
        ],
    ),
    BenchmarkCase(
        id="cr_02",
        competency="conflict_resolution",
        description="Numerical updates and corrections",
        context_chunks=[
            "The server has 16GB of RAM.",
            "We upgraded the server — it now has 64GB of RAM.",
            "Our API handles about 1000 requests per second.",
            "After optimization, our API now handles 5000 requests per second.",
            "The database has 500,000 records.",
            "After the data import, the database now contains 2.3 million records.",
            "Our team size is 8 people.",
            "We just hired 3 more engineers, bringing the team to 11.",
        ],
        queries=[
            ("How much RAM does the server have?", "64GB"),
            ("How many requests per second does the API handle?", "5000"),
            ("How many records are in the database?", "2.3 million"),
            ("How many people are on the team?", "11"),
        ],
    ),
    BenchmarkCase(
        id="cr_03",
        competency="conflict_resolution",
        description="Contradictory sources — temporal ordering matters",
        context_chunks=[
            "Meeting notes (Jan 5): The product launch date is set for March 1st.",
            "Email from CEO (Jan 20): Due to supply chain issues, launch is delayed to April 15th.",
            "Slack message (Feb 1): Good news — supply chain cleared up. Launch moved back to March 15th.",
            "Team standup (Feb 10): Final confirmation — we are launching on March 15th. All teams aligned.",
            "Meeting notes (Jan 5): Budget for Q1 marketing is $50,000.",
            "Budget revision (Jan 25): Q1 marketing budget increased to $75,000 after board approval.",
            "CFO memo (Feb 5): Emergency cost cuts — Q1 marketing budget reduced to $60,000.",
        ],
        queries=[
            ("When is the product launch date?", "March 15th"),
            ("What is the Q1 marketing budget?", "$60,000"),
        ],
    ),
]


# =============================================================================
# COMPETENCY 5 (BONUS): SEMANTIC SIMILARITY
# Test: Can the system find semantically similar but differently worded content?
# =============================================================================

SEMANTIC_SIMILARITY = [
    BenchmarkCase(
        id="ss_01",
        competency="semantic_similarity",
        description="Paraphrased queries — testing embedding quality",
        context_chunks=[
            "The quarterly revenue for our SaaS product was $1.2 million, representing a 25% increase year over year.",
            "Customer churn rate decreased from 8% to 5.5% after implementing the new onboarding flow.",
            "The mobile app crashed 342 times last week, mostly due to a memory leak in the image loading module.",
            "We signed an enterprise contract with Acme Corp for $500K annually.",
            "The average response time for our API dropped from 200ms to 45ms after migrating to Rust.",
        ],
        queries=[
            # Paraphrased queries — not using exact words from the chunks
            ("How much money did the SaaS product make last quarter?", "$1.2 million"),
            ("Did customer retention improve after the new user setup process?", "churn rate decreased from 8% to 5.5%"),
            ("Were there any stability issues with the mobile application?", "crashed 342 times"),
            ("What deal did we close with Acme Corp?", "$500K annually"),
            ("How fast is the API after the technology migration?", "45ms"),
        ],
    ),
]


# =============================================================================
# ALL DATASETS
# =============================================================================

ALL_BENCHMARKS = {
    "accurate_retrieval": ACCURATE_RETRIEVAL,
    "test_time_learning": TEST_TIME_LEARNING,
    "long_range_understanding": LONG_RANGE_UNDERSTANDING,
    "conflict_resolution": CONFLICT_RESOLUTION,
    "semantic_similarity": SEMANTIC_SIMILARITY,
}

ALL_CASES = []
for cases in ALL_BENCHMARKS.values():
    ALL_CASES.extend(cases)
