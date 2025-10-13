# Network Proxy Stress Testing Tool (NPT)

A comprehensive network proxy stress testing tool built with Rust, featuring realistic user behavior simulation and real-time monitoring capabilities.

## Features

### Core Functionality
- **Client-Server Architecture**: Separate client and server applications for distributed testing
- **Realistic Traffic Simulation**: Models real-world user activities including:
  - Web browsing with TLS handshakes and HTTP request/response cycles
  - File downloads and uploads with chunked transfer
  - Gaming traffic with UDP packets and fragmentation
- **Concurrent User Simulation**: Supports configurable number of simultaneous users
- **Configurable Test Parameters**: Customizable packet sizes, connection intervals, and behavior patterns

### Real-time Monitoring
- **Text-based User Interface (TUI)**: Live dashboards for both client and server
- **Connection Tracking**: Real-time count of active TCP and UDP connections
- **Throughput Visualization**: Live charts showing upload/download speeds separately for TCP and UDP
- **Latency Monitoring**: Real-time request latency tracking and visualization
- **Error Statistics**: Cumulative failed request counts and error rates

### Comprehensive Reporting
- **Multiple Output Formats**: JSON, CSV, and HTML reports
- **Detailed Statistics**: Connection success rates, throughput distribution, latency percentiles
- **Performance Metrics**: Average and peak throughput, P50/P95/P99 latencies, error rates
- **Raw Data Export**: Optional inclusion of all collected metrics for detailed analysis

## Architecture

```
npt/
├── shared/          # Common protocols, metrics, and utilities
├── client/          # Client application with user simulation
├── server/          # Server application with traffic handling
└── README.md
```

### Components

#### Shared Library
- **Protocol Definitions**: Message types and communication formats
- **Metrics Collection**: Real-time statistics gathering and aggregation
- **Configuration Management**: Test parameters and behavior settings
- **Report Generation**: Comprehensive test result analysis

#### Client Application
- **User Simulator**: Concurrent user behavior modeling
- **TCP Client**: Web browsing and file transfer simulation
- **UDP Client**: Gaming packet exchange with fragmentation
- **Client TUI**: Real-time monitoring dashboard

#### Server Application
- **TCP Server**: Handles web browsing and file transfer requests
- **UDP Server**: Processes gaming packets with realistic responses
- **Server TUI**: Connection and performance monitoring dashboard

## Installation

### Prerequisites
- Rust 1.70 or later
- tokio async runtime
- ratatui for terminal UI

### Building from Source
```bash
git clone <repository-url>
cd npt
cargo build --release
```

### Quick Start Example
```bash
# Terminal 1: Start the server
./target/release/server --address 0.0.0.0:8080

# Terminal 2: Run a quick test with 50 users for 60 seconds
./target/release/client --server 127.0.0.1:8080 --users 50 --duration 60
```

## Usage

### Starting the Server
```bash
# Basic server startup
./target/release/server

# Custom configuration
./target/release/server --address 0.0.0.0:8080 --tcp-port 8080 --udp-port 8081

# Headless mode (no TUI)
./target/release/server --no-tui

# Custom report output
./target/release/server --output ./server_metrics --format html
```

### Running the Client
```bash
# Basic client test
./target/release/client --server 127.0.0.1:8080

# Custom test parameters
./target/release/client --server 192.168.1.100:8080 --users 200 --duration 600

# Headless mode
./target/release/client --no-tui --users 50 --duration 300

# Custom output format
./target/release/client --output ./test_results --format csv
```

### Command Line Options

#### Server Options
- `--address, -a`: Server bind address (default: 0.0.0.0:8080)
- `--tcp-port`: TCP server port (default: same as main address)
- `--udp-port`: UDP server port (default: same as main address)
- `--no-tui`: Run without terminal interface
- `--output, -o`: Output path for metrics report (default: ./server_report)
- `--format, -f`: Report format: json, csv, html (default: json)

#### Client Options
- `--server, -s`: Server address to connect to (default: 127.0.0.1:8080)
- `--users, -u`: Number of concurrent users (default: 100)
- `--duration, -d`: Test duration in seconds (default: 300)
- `--no-tui`: Run without terminal interface
- `--output, -o`: Output path for test report (default: ./client_report)
- `--format, -f`: Report format: json, csv, html (default: json)

## Traffic Simulation Details

### Web Browsing Simulation
1. **TLS Handshake**: Client sends 400-2,000 byte packet
2. **Server Response**: 2,000-4,000 byte response
3. **HTTP Rounds**: Random number of request/response cycles
   - Client requests: 1 KB to 1 MB chunks
   - Server responses: 4 KB to 5 MB chunks

### File Transfer Simulation
- **Download**: Client requests file, server sends in chunks
- **Upload**: Client sends file in chunks, server acknowledges
- Configurable chunk sizes and transfer patterns

### Gaming Simulation (UDP)
- Random packet sizes with fragmentation support
- Configurable packets per second
- Realistic network jitter and packet loss simulation
- Session-based tracking with cleanup

## Configuration

### Default Behavior Weights
- Web Browsing: 40%
- File Download: 30%
- File Upload: 20%
- Gaming: 10%

### Default Packet Sizes
- TCP Handshake: 400-2,000 bytes
- TCP Response: 2,000-4,000 bytes
- UDP Gaming: 64-1,400 bytes
- File Chunks: 1 KB-1 MB

### Think Time Ranges
- User think time: 100ms-5s between activities
- Connection intervals: 50ms-2s between connections

## Monitoring and Metrics

### Real-time TUI Features
- **Connection Gauges**: Visual representation of active connections
- **Throughput Charts**: Live line charts with separate TCP/UDP tracking
- **Latency Graphs**: Request latency visualization over time
- **Error Counters**: Cumulative error statistics
- **Sparklines**: Historical connection and throughput trends

### Key Performance Indicators
- Active TCP/UDP connection counts
- Real-time throughput (Mbps) with upload/download breakdown
- Request latency percentiles (P50, P95, P99)
- Connection success/failure rates
- Error distribution by protocol type

## Report Analysis

### Generated Reports Include
- **Test Summary**: Duration, connection counts, data transfer totals
- **Performance Metrics**: Throughput statistics, latency analysis
- **Connection Statistics**: Success rates, duration analysis
- **Error Analysis**: Error rates and distribution
- **Optional Raw Data**: All collected metrics for custom analysis

### Report Formats
- **JSON**: Machine-readable structured data
- **CSV**: Spreadsheet-compatible tabular data
- **HTML**: Human-readable formatted report with visualizations

## Technical Implementation

### Async Architecture
- Built on tokio async runtime for high concurrency
- Non-blocking I/O for optimal performance
- Concurrent user simulation with semaphore-based throttling

### Protocol Design
- JSON-based message serialization
- Length-prefixed TCP messages for reliable delivery
- UDP fragmentation support for large packets
- Session tracking with UUID-based identification

### Metrics Collection
- Lock-free data structures for high-performance collection
- Real-time sampling with configurable intervals
- Memory-efficient rolling windows for historical data
- Thread-safe access for concurrent updates

## Performance Considerations

### Scalability
- Supports thousands of concurrent connections
- Memory-efficient metrics storage
- Configurable resource limits and timeouts
- Automatic cleanup of inactive sessions

### Resource Management
- Connection pooling and reuse
- Bounded memory usage for metrics history
- Graceful degradation under high load
- Comprehensive error handling and recovery

## Troubleshooting

### Common Issues
1. **Connection Refused**: Ensure server is running and accessible
2. **High Memory Usage**: Reduce concurrent users or test duration
3. **TUI Display Issues**: Check terminal size and color support
4. **Permission Errors**: Ensure write access for report output directory
5. **Unrealistic Throughput Values**: Fixed in latest version - throughput calculation now uses proper delta values

### Debug Mode
Enable detailed logging with:
```bash
RUST_LOG=debug ./target/release/client
RUST_LOG=debug ./target/release/server
```

### Version History
- **v0.1.1**: Fixed throughput calculation algorithm to show realistic bandwidth values
- **v0.1.0**: Initial release with complete feature set

## Contributing

Contributions are welcome! Please ensure:
- Code follows Rust conventions and formatting
- Tests pass for all components
- Documentation is updated for new features
- Performance impact is considered for high-load scenarios

## License

This project is licensed under the MIT License - see the LICENSE file for details.