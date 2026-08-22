package tarno

import (
	"context"
	"encoding/json"
	"net"
	"os"
)

const MistralBaseURL = "https://api.mistral.ai/v1"

const SocketPath = "/run/tarnod.sock"

type TarnoD struct {
	provider Provider
}

func New() *TarnoD {
	d := &TarnoD{}
	if key := os.Getenv("MISTRAL_API_KEY"); key != "" {
		d.provider = NewMistralProvider(key)
	}
	return d
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
		if d.provider == nil {
			return Response{Ok: false, Error: "mistral not configured (set MISTRAL_API_KEY)"}
		}
		answer, err := d.provider.Query(req.Text)
		if err != nil {
			return Response{Ok: false, Error: err.Error()}
		}
		return Response{Ok: true, Data: answer}

	default:
		return Response{Ok: false, Error: "unknown command: " + req.Cmd}
	}
}
