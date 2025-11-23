package main

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	statsService "github.com/xtls/xray-core/app/stats/command"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

// ================= CONFIG =================

var (
	scrapeInterval = 5 * time.Second
	failInterval   = 15 * time.Second
	rpcTimeout     = 3 * time.Second
)

// ================= METRICS =================

var (
	xrayTraffic = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "xray_traffic_bytes",
			Help: "Xray traffic statistics (user/inbound/outbound)",
		},
		[]string{"type", "name", "direction"},
	)

	xrayUserIPOnline = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "xray_user_ip_online",
			Help: "User online status per IP (1=online)",
		},
		[]string{"name", "ip"},
	)

	xrayUp = prometheus.NewGauge(
		prometheus.GaugeOpts{
			Name: "xray_up",
			Help: "Whether Xray is reachable",
		},
	)
)

// ================= MAIN =================

func main() {
	log.Printf("Starting Xray exporter %s...\n", Version)

	// Prometheus registry
	reg := prometheus.NewRegistry()
	reg.MustRegister(xrayTraffic)
	reg.MustRegister(xrayUserIPOnline)
	reg.MustRegister(xrayUp)

	// gRPC connection
	conn, err := grpc.NewClient(AppConfig.XrayApi, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		log.Fatal("Connect to Xray failed:", err)
	}
	defer conn.Close()
	client := statsService.NewStatsServiceClient(conn)

	// Handle shutdown
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	// Start scrape loop
	go scrapeLoop(ctx, client)

	// Start HTTP server
	http.Handle("/metrics", promhttp.HandlerFor(reg, promhttp.HandlerOpts{}))
	addr := fmt.Sprintf(":%d", AppConfig.Port)
	log.Printf("Exporter listening on %s/metrics\n", addr)
	log.Fatal(http.ListenAndServe(addr, nil))
}

// ================= SCRAPE LOOP =================

func scrapeLoop(ctx context.Context, client statsService.StatsServiceClient) {
	log.Println("Scrape loop started (single-thread mode)")

	failCount := 0

	// Initial delay
	time.Sleep(2 * time.Second)

	for {
		select {
		case <-ctx.Done():
			log.Println("Scrape loop stopped")
			return
		default:
			err := scrapeTraffic(client)
			if err != nil {
				failCount++
				xrayUp.Set(0)
				log.Println("scrapeTraffic error:", err)
			} else {
				failCount = 0
				xrayUp.Set(1)
			}

			// Adjust sleep
			sleep := scrapeInterval
			if failCount >= 3 {
				sleep = failInterval
			}
			time.Sleep(sleep)
		}
	}
}

// ================= SCRAPE FUNCTIONS =================

func scrapeTraffic(c statsService.StatsServiceClient) error {
	ctx, cancel := context.WithTimeout(context.Background(), rpcTimeout)
	defer cancel()

	resp, err := c.QueryStats(ctx, &statsService.QueryStatsRequest{
		Pattern: "",
		Reset_:  false,
	})
	if err != nil {
		return err
	}

	// Reset metrics
	xrayTraffic.Reset()
	xrayUserIPOnline.Reset()

	// Extract users and parse traffic
	users := make(map[string]struct{})

	for _, stat := range resp.Stat {
		if strings.HasPrefix(stat.Name, "user>>>") {
			user, ok := parseUser(stat.Name)
			if ok {
				users[user] = struct{}{}
			}
		}
		parseAndSetTraffic(stat.Name, stat.Value)
	}
	// Query online IPs for each user (serial)
	for user := range users {
		ctx2, cancel2 := context.WithTimeout(context.Background(), rpcTimeout)
		ipResp, err := c.GetStatsOnlineIpList(ctx2, &statsService.GetStatsRequest{
			Name: "user>>>" + user + ">>>online",
		})
		cancel2()
		if err != nil {
			log.Printf("GetStatsOnlineIpList error for user %s: %v", user, err)
			continue
		}

		for ip := range ipResp.Ips {
			xrayUserIPOnline.WithLabelValues(user, ip).Set(1)
		}
	}

	return nil
}

// ================= PARSERS =================

func parseAndSetTraffic(name string, value int64) {
	parts := strings.Split(name, ">>>")
	if len(parts) < 4 {
		return
	}
	typ := parts[0]
	nameLabel := parts[1]
	direction := parts[3]

	xrayTraffic.WithLabelValues(typ, nameLabel, direction).Set(float64(value))
}

func parseUser(statName string) (string, bool) {
	parts := strings.Split(statName, ">>>")
	if len(parts) < 2 {
		return "", false
	}
	return parts[1], true
}
