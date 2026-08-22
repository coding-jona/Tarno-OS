package main

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"strings"

	"github.com/coding-jona/Tarno-OS/tarno"
)

func send(req tarno.Request) (tarno.Response, error) {
	conn, err := net.Dial("unix", tarno.SocketPath)
	if err != nil {
		return tarno.Response{}, err
	}
	defer func() { _ = conn.Close() }()

	if err := json.NewEncoder(conn).Encode(req); err != nil {
		return tarno.Response{}, err
	}

	var resp tarno.Response
	if err := json.NewDecoder(conn).Decode(&resp); err != nil {
		return tarno.Response{}, err
	}
	return resp, nil
}

func usage() {
	fmt.Fprintln(os.Stderr, "usage: tarnoctl status")
	fmt.Fprintln(os.Stderr, "       tarnoctl ai <question>")
}

func main() {
	if len(os.Args) < 2 {
		usage()
		os.Exit(1)
	}

	var req tarno.Request
	switch os.Args[1] {
	case "status":
		req = tarno.Request{Cmd: "status"}
	case "ai":
		if len(os.Args) < 3 {
			usage()
			os.Exit(1)
		}
		req = tarno.Request{Cmd: "ai", Text: strings.Join(os.Args[2:], " ")}
	default:
		usage()
		os.Exit(1)
	}

	resp, err := send(req)
	if err != nil {
		fmt.Fprintln(os.Stderr, "tarnod unreachable:", err)
		os.Exit(1)
	}

	if !resp.Ok {
		fmt.Fprintln(os.Stderr, resp.Error)
		os.Exit(1)
	}
	fmt.Println(resp.Data)
}
