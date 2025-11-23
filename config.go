package main

import (
	"os"
	"strconv"
)

type Config struct {
	XrayApi string
	Port    uint16
}

var AppConfig = &Config{
	XrayApi: func() string {
		if v := os.Getenv("XRAY_API"); v != "" {
			return v
		}
		return "127.0.0.1:8080"
	}(),
	Port: func() uint16 {
		if v := os.Getenv("PORT"); v != "" {
			if p, err := strconv.ParseUint(v, 10, 16); err == nil {
				return uint16(p)
			}
		}
		return 9100
	}(),
}
