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
#   errors      Show recent error logs (use -f to follow)
#   reload      Send SIGHUP to reopen log files
#

set -e

SERVICE_NAME="comelit-hub-hap"
PLIST_NAME="com.comelit.hub.hap"
LOG_FILE="/var/log/comelit-hub-hap.log"
ERR_FILE="/var/log/comelit-hub-hap.err"

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
    macos)  PID_FILE="/var/lib/comelit-hub-hap/comelit-hub-hap.pid" ;;
    *)      PID_FILE="/var/lib/comelit-hub-hap/comelit-hub-hap.pid" ;;
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

# Show logs
do_logs() {
    FOLLOW=""
    LINES="50"

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
            *)
                shift
                ;;
        esac
    done

    print_info "Showing logs from ${LOG_FILE}:"
    echo ""

    if [ ! -f "$LOG_FILE" ]; then
        print_warn "Log file not found: ${LOG_FILE}"

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

    if [ -n "$FOLLOW" ]; then
        tail -f "$LOG_FILE"
    else
        tail -n "$LINES" "$LOG_FILE"
    fi
}

# Show error logs
do_errors() {
    FOLLOW=""
    LINES="50"

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
            *)
                shift
                ;;
        esac
    done

    print_info "Showing error logs from ${ERR_FILE}:"
    echo ""

    if [ ! -f "$ERR_FILE" ]; then
        print_warn "Error log file not found: ${ERR_FILE}"
        return
    fi

    if [ -n "$FOLLOW" ]; then
        tail -f "$ERR_FILE"
    else
        tail -n "$LINES" "$ERR_FILE"
    fi
}

# Send SIGHUP to reload logs
do_reload() {
    check_root
    print_info "Sending SIGHUP to ${SERVICE_NAME} to reopen log files..."

    case "$OS" in
        linux)
            if systemctl is-active --quiet "$SERVICE_NAME"; then
                systemctl kill -s HUP "$SERVICE_NAME"
                print_success "SIGHUP sent"
            else
                print_error "Service is not running"
                exit 1
            fi
            ;;
        macos)
            if [ -f "$PID_FILE" ]; then
                PID=$(cat "$PID_FILE")
                if ps -p "$PID" > /dev/null 2>&1; then
                    kill -HUP "$PID"
                    print_success "SIGHUP sent to PID $PID"
                else
                    print_error "Process not running (stale PID file)"
                    exit 1
                fi
            else
                # Try to find the process
                PID=$(pgrep -f "comelit-hub-hap" | head -1)
                if [ -n "$PID" ]; then
                    kill -HUP "$PID"
                    print_success "SIGHUP sent to PID $PID"
                else
                    print_error "Could not find running process"
                    exit 1
                fi
            fi
            ;;
        *)
            print_error "Unsupported operating system"
            exit 1
            ;;
    esac
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
    logs        Show recent logs
    errors      Show recent error logs
    reload      Send SIGHUP to reopen log files (for log rotation)

Log options:
    -f, --follow    Follow log output (like tail -f)
    -n, --lines N   Show last N lines (default: 50)

Examples:
    $(basename "$0") start
    $(basename "$0") logs -f
    $(basename "$0") logs -n 100
    $(basename "$0") errors -f

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
    errors)
        shift
        do_errors "$@"
        ;;
    reload)
        do_reload
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage
        exit 1
        ;;
esac
