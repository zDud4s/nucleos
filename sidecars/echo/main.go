package main

import (
	"encoding/json"
	"log"
	"net/http"
	"os"
)

func buildMux() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("/health", healthHandler)
	return mux
}

func healthHandler(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
}

func main() {
	// Stateless, so the orderly shutdown when the daemon is gone is simply to exit.
	watchLifeline(os.Getenv, os.Stdin, func() { os.Exit(0) })
	log.Println("echo-sidecar listening on 127.0.0.1:8792")
	if err := http.ListenAndServe("127.0.0.1:8792", buildMux()); err != nil {
		log.Fatal(err)
	}
}
