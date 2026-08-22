package main

import (
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"

	"github.com/coding-jona/Tarno-OS/tarno"
)

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	d := tarno.New()
	log.Println("tarnod listening on", tarno.SocketPath)
	if err := d.Run(ctx); err != nil {
		log.Fatal(err)
	}
}
