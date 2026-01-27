#!/bin/sh
#
# comelit-hub-ctl.sh - Management script for Comelit HUB HAP service
#
# Usage: comelit-hub-ctl.sh <command>
#
# Commands:
#   start       Start the service
#   stop        Stop the service
#   restart     Restart the service
#   status      Show service status
#   logs        Show recent logs (use -f to follow)
#

set -e

SERVICE_NAME="comelit-hub-hap"
PLIST_NAME="com.comelit.hub.hap"
LOG_DIR="/var/log/comelit-hub-hap"
DATA_DIR="/var/lib/comelit-hub-hap"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Detect OS
detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "macos" ;;
        *)       echo "unknown" ;;
    esac
}

OS=$(detect_os)

# Set platform-specific PID file location
case "$OS" in
    linux)  PID_FILE="/run/comelit-hub-hap/comelit-hub-hap.pid" ;;
    macos)  PID_FILE="$DATA_DIR/comelit-hub-hap.pid" ;;
    *)      PID_FILE="$DATA_DIR/comelit-hub-hap.pid" ;;
esac

# Print colored output
print_info() {
    printf "${GREEN}▶${NC} %s\n" "$1"
}

print_warn() {
    printf "${YELLOW}⚠${NC} %s\n" "$1"
}

print_error() {
    printf "${RED}✗${NC} %s\n" "$1"
}

print_success() {
    printf "${GREEN}✔${NC} %s\n" "$1"
}

# Check if running as root
check_root() {
    if [ "$(id -u)" -ne 0 ]; then
        print_error "This command must be run as root (use sudo)"
        exit 1
    fi
}

# Start the service
do_start() {
    check_root
    print_info "Starting ${SERVICE_NAME}..."

    case "$OS" in
        linux)
            if systemctl is-active --quiet "$SERVICE_NAME"; then
                print_warn "Service is already running"
            else
                systemctl start "$SERVICE_NAME"
                print_success "Service started"
            fi
            ;;
        macos)
            if launchctl list | grep -q "$PLIST_NAME"; then
                print_warn "Service is already loaded"
            else
                launchctl load -w "/Library/LaunchDaemons/${PLIST_NAME}.plist"
                print_success "Service started"
            fi
            ;;
        *)
            print_error "Unsupported operating system"
            exit 1
            ;;
    esac
}

# Stop the service
do_stop() {
    check_root
    print_info "Stopping ${SERVICE_NAME}..."

    case "$OS" in
        linux)
            if systemctl is-active --quiet "$SERVICE_NAME"; then
                systemctl stop "$SERVICE_NAME"
                print_success "Service stopped"
            else
                print_warn "Service is not running"
            fi
            ;;
        macos)
            if launchctl list | grep -q "$PLIST_NAME"; then
                launchctl unload "/Library/LaunchDaemons/${PLIST_NAME}.plist"
                print_success "Service stopped"
            else
                print_warn "Service is not loaded"
            fi
            ;;
        *)
            print_error "Unsupported operating system"
            exit 1
            ;;
    esac
}

# Restart the service
do_restart() {
    check_root
    print_info "Restarting ${SERVICE_NAME}..."

    case "$OS" in
        linux)
            systemctl restart "$SERVICE_NAME"
            print_success "Service restarted"
            ;;
        macos)
            if launchctl list | grep -q "$PLIST_NAME"; then
                launchctl unload "/Library/LaunchDaemons/${PLIST_NAME}.plist"
            fi
            sleep 1
            launchctl load -w "/Library/LaunchDaemons/${PLIST_NAME}.plist"
            print_success "Service restarted"
            ;;
        *)
            print_error "Unsupported operating system"
            exit 1
            ;;
    esac
}

# Show service status
do_status() {
    print_info "Status of ${SERVICE_NAME}:"
    echo ""

    case "$OS" in
        linux)
            systemctl status "$SERVICE_NAME" --no-pager || true
            ;;
        macos)
            if launchctl list | grep -q "$PLIST_NAME"; then
                echo "Service: ${GREEN}loaded${NC}"
                launchctl list | grep "$PLIST_NAME"

                # Show PID if available
                if [ -f "$PID_FILE" ]; then
                    PID=$(cat "$PID_FILE")
                    if ps -p "$PID" > /dev/null 2>&1; then
                        echo "PID: $PID (running)"
                    else
                        echo "PID: $PID (stale pid file)"
                    fi
                fi
            else
                printf "Service: ${RED}not loaded${NC}\n"
            fi
            ;;
        *)
            print_error "Unsupported operating system"
            exit 1
            ;;
    esac
}

# Find the most recent log file
find_latest_log() {
    if [ -d "$LOG_DIR" ]; then
        # Find the most recently modified .log file
        ls -t "$LOG_DIR"/*.log 2>/dev/null | head -1
    fi
}

# Show logs
do_logs() {
    FOLLOW=""
    LINES="50"
    ALL_FILES=""

    # Parse arguments
    while [ $# -gt 0 ]; do
        case "$1" in
            -f|--follow)
                FOLLOW="yes"
                shift
                ;;
            -n|--lines)
                LINES="$2"
                shift 2
                ;;
            -a|--all)
                ALL_FILES="yes"
                shift
                ;;
            *)
                shift
                ;;
        esac
    done

    if [ ! -d "$LOG_DIR" ]; then
        print_warn "Log directory not found: ${LOG_DIR}"

        # On Linux, try journalctl as fallback
        if [ "$OS" = "linux" ]; then
            print_info "Trying journalctl instead..."
            if [ -n "$FOLLOW" ]; then
                journalctl -u "$SERVICE_NAME" -f
            else
                journalctl -u "$SERVICE_NAME" -n "$LINES" --no-pager
            fi
        fi
        return
    fi

    if [ -n "$ALL_FILES" ]; then
        # Show all log files concatenated
        print_info "Showing all logs from ${LOG_DIR}:"
        echo ""
        if [ -n "$FOLLOW" ]; then
            # Follow all files
            tail -f "$LOG_DIR"/*.log 2>/dev/null
        else
            # Show last N lines from all files combined, sorted by time
            cat "$LOG_DIR"/*.log 2>/dev/null | tail -n "$LINES"
        fi
    else
        # Show only the latest log file
        LATEST_LOG=$(find_latest_log)
        if [ -z "$LATEST_LOG" ]; then
            print_warn "No log files found in ${LOG_DIR}"
            return
        fi

        print_info "Showing logs from ${LATEST_LOG}:"
        echo ""

        if [ -n "$FOLLOW" ]; then
            tail -f "$LATEST_LOG"
        else
            tail -n "$LINES" "$LATEST_LOG"
        fi
    fi
}

# List all log files
do_list_logs() {
    print_info "Log files in ${LOG_DIR}:"
    echo ""

    if [ ! -d "$LOG_DIR" ]; then
        print_warn "Log directory not found: ${LOG_DIR}"
        return
    fi

    ls -lh "$LOG_DIR"/*.log 2>/dev/null || print_warn "No log files found"
}

# Reset the service configuration
do_reset() {
    print_info "Resetting service configuration..."
    echo ""

    if [ ! -d "$LOG_DIR" ]; then
        print_warn "Log directory not found: ${LOG_DIR}"
        return
    fi

    rm -f "$LOG_DIR/comelit-hub-hap.log"
    rm -f "$LOG_DIR/comelit-hub-hap.err"
    touch "$LOG_DIR/comelit-hub-hap.log"
    touch "$LOG_DIR/comelit-hub-hap.err"
    chmod 644 "$LOG_DIR/comelit-hub-hap.log"
    chmod 644 "$LOG_DIR/comelit-hub-hap.err"
    rm -rf "$DATA_DIR/data"

    systemctl daemon-reload
    systemctl enable comelit-hub-hap

    print_info "Service configuration reset successfully"
}

# Show usage
usage() {
    cat << EOF
Comelit HUB HAP Service Control

Usage: $(basename "$0") <command> [options]

Commands:
    start       Start the service
    stop        Stop the service
    restart     Restart the service
    status      Show service status
    logs        Show recent logs from the latest log file
    list-logs   List all log files
    reset       Reset the service configuration

Log options:
    -f, --follow    Follow log output (like tail -f)
    -n, --lines N   Show last N lines (default: 50)
    -a, --all       Show logs from all files (not just the latest)

Examples:
    $(
basename "$0") start
    $(basename "$0") logs -f
    $(basename "$0") logs -n 100
    $(basename "$0") logs -a -n 200
    $(basename "$0") list-logs

Note: Log rotation is handled automatically by the application.
      Old log files are cleaned up based on the max-log-files setting.

EOF
}

# Main
case "${1:-}" in
    start)
        do_start
        ;;
    stop)
        do_stop
        ;;
    restart)
        do_restart
        ;;
    status)
        do_status
        ;;
    logs)
        shift
        do_logs "$@"
        ;;
    list-logs)
        do_list_logs
        ;;
    reset)
        do_reset
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage
        exit 1
        ;;
esac
