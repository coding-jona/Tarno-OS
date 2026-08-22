package tarno

type Provider interface {
	Query(text string) (string, error)
}

type Request struct {
	Cmd  string `json:"cmd"`
	Text string `json:"text,omitempty"`
}

type Response struct {
	Ok    bool   `json:"ok"`
	Data  string `json:"data,omitempty"`
	Error string `json:"error,omitempty"`
}
