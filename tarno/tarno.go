package tarno

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

const MistralBaseURL = "https://api.mistral.ai/v1"

const SocketPath = "/run/tarnod.sock"

// ConfigDir/APIKeyPath: a persistent home for the Mistral API key beyond
// the MISTRAL_API_KEY env var, so it can be set from tarno-settings'
// AI tab instead of only at tarnod's own OpenRC-service startup (nothing
// in the shipped image ever set that env var, so the AI tab was
// unreachable out of the box despite existing).
//
// The reference project this whole assistant is modeled on
// (coding-jona/tarno) deliberately never writes API keys into its config
// file - it resolves them through a SecretsVault (OS keyring primary,
// an encrypted-file fallback tier) instead, precisely so a key isn't
// sitting in plaintext config. This image has no desktop keyring daemon
// (no gnome-keyring/kwallet - it's a minimal Wayland/OpenRC image, not a
// full desktop stack), so there's no keyring tier to hook into yet. A
// root-owned, 0600 file is the closest practical equivalent to their own
// documented fallback tier: tarnod already runs as root, and the file is
// unreadable to the unprivileged "user" account tarno-settings runs as -
// strictly better isolated than the MISTRAL_API_KEY env var it replaces
// (an env var is visible to any process sharing the same uid, or to root
// via /proc/<pid>/environ regardless). See docs/tarno-ai-roadmap.md for
// the rest of the plan this is Phase 1 of.
const ConfigDir = "/etc/tarnod"

var APIKeyPath = filepath.Join(ConfigDir, "mistral_api_key")

var errEmptyAPIKey = errors.New("api key is empty")

type TarnoD struct {
	mu       sync.RWMutex
	provider Provider
}

func New() *TarnoD {
	d := &TarnoD{}
	key := os.Getenv("MISTRAL_API_KEY")
	if key == "" {
		key = readAPIKeyFile()
	}
	if key != "" {
		d.provider = NewMistralProvider(key)
	}
	return d
}

func readAPIKeyFile() string {
	data, err := os.ReadFile(APIKeyPath)
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(data))
}

// setAPIKey persists key to APIKeyPath and swaps the live provider in,
// with no tarnod restart needed - tarno-settings can call this directly
// over the socket the moment a key is entered.
func (d *TarnoD) setAPIKey(key string) error {
	key = strings.TrimSpace(key)
	if key == "" {
		return errEmptyAPIKey
	}
	if err := os.MkdirAll(ConfigDir, 0o700); err != nil {
		return err
	}
	if err := os.WriteFile(APIKeyPath, []byte(key), 0o600); err != nil {
		return err
	}

	d.mu.Lock()
	d.provider = NewMistralProvider(key)
	d.mu.Unlock()
	return nil
}

func (d *TarnoD) currentProvider() Provider {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return d.provider
}

// Run listens on SocketPath until ctx is cancelled.
func (d *TarnoD) Run(ctx context.Context) error {
	_ = os.Remove(SocketPath)

	l, err := net.Listen("unix", SocketPath)
	if err != nil {
		return err
	}
	defer func() { _ = l.Close() }()

	// tarnod runs as root; without this the socket inherits the
	// process umask (typically 0755) and Unix-socket connect() needs
	// *write* permission, so tarno-settings/tarnoctl running as the
	// live user get EACCES - confirmed on a real boot ("tarnod
	// unreachable: [Errno 13] Permission denied"). Single-user image,
	// root permanently locked - world-writable is the whole point.
	if err := os.Chmod(SocketPath, 0o666); err != nil {
		return err
	}

	go func() {
		<-ctx.Done()
		_ = l.Close()
	}()

	for {
		conn, err := l.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return err
		}
		go d.handle(conn)
	}
}

func (d *TarnoD) handle(conn net.Conn) {
	defer func() { _ = conn.Close() }()

	var req Request
	if err := json.NewDecoder(conn).Decode(&req); err != nil {
		return
	}
	_ = json.NewEncoder(conn).Encode(d.dispatch(req))
}

func (d *TarnoD) dispatch(req Request) Response {
	switch req.Cmd {
	case "status":
		return Response{Ok: true, Data: "tarnod running"}

	case "ai":
		provider := d.currentProvider()
		if provider == nil {
			return Response{Ok: false, Error: "mistral not configured - set an API key in Tarno Settings' AI tab"}
		}
		answer, err := provider.Query(req.Text)
		if err != nil {
			return Response{Ok: false, Error: err.Error()}
		}
		return Response{Ok: true, Data: answer}

	case "ai_status":
		if d.currentProvider() == nil {
			return Response{Ok: true, Data: "not configured"}
		}
		return Response{Ok: true, Data: "configured"}

	case "set_api_key":
		if err := d.setAPIKey(req.Text); err != nil {
			return Response{Ok: false, Error: err.Error()}
		}
		return Response{Ok: true, Data: "api key saved"}

	default:
		return Response{Ok: false, Error: "unknown command: " + req.Cmd}
	}
}
