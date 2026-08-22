package tarno

import mistral "github.com/gage-technologies/mistral-go"

/*
 {
    "codestral-2508": 2.08,
    "codestral-latest": 2.08,
    "codestral-embed": 1.00,
    "devstral-2512": 0.83,
    "devstral-latest": 0.83,
    "labs-leanstral-1-5-1": 0.63,
    "labs-leanstral-1-5": 0.63,
    "magistral-medium-2509": 0.08,
    "magistral-medium-latest": 0.08,
    "magistral-small-2509": 0.03,
    "magistral-small-latest": 0.03,
    "ministral-14b-2512": 0.50,
    "ministral-3b-2512": 12.50,
    "ministral-8b-2512": 3.13,
    "mistral-embed-2312": 1.00,
    "mistral-large-2512": 0.07,
    "mistral-large-latest": 0.07,
    "mistral-medium-2505": 0.42,
    "mistral-medium-2508": 0.38,
    "mistral-medium-latest": 0.83,
    "mistral-moderation-2603": 1.67,
    "mistral-small-2506": 5.00,
    "mistral-small-2603": 0.83,
    "mistral-small-latest": 0.83,
    "open-mistral-nemo": 0.50,
    "voxtral-mini-2507": 1.00,
    "voxtral-mini-2602": 1.00,
    "voxtral-mini-transcribe-realtime-2602": 1.00,
    "voxtral-mini-tts-2603": 1.00,
}
*/

type MistralProvider struct{}

func NewMistralProvider(apikey string) {
	mistral.NewMistralClientDefault(apikey)

}
