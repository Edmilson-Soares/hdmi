# hdmi — desenhar no monitor HDMI com Fogu

Abre uma janela em tela cheia no monitor ligado à porta HDMI e desenha nela a
partir do Fogu: formas com anti-aliasing, degradês, transparência, texto com as
fontes do Windows, teclado e mouse. O `main.fg` mostra uma paisagem animada
(bolas quicando, relógio, painel com os dados do monitor).

```
foguhdmi/       lib Rust (cdylib) com a API C: monitores, janela, desenho
native/         foguhdmi.dll (gerado pelo build)
tela.fg         wrapper Fogu: monitores(), abrir(), Tela
tela.test.fg    testes (fogu test); usam canvas fora da tela, sem janela
main.fg         exemplo animado no monitor HDMI
html.fg         exemplo de HTML no monitor HDMI (no canvas ou navegador ao vivo)
som.fg          wrapper Fogu: áudio no HDMI e texto para fala
som.test.fg     testes do áudio (os que tocam usam volume 0 na saída padrão)
audio.fg        exemplo: fala um texto pelo HDMI com legenda no monitor
```

<!-- fogu:plataformas -->
## Plataformas, binários e pré-requisitos

Ao importar `tela.fg / som.fg`, a lib reconhece o sistema e a arquitetura
(`system.info.os`/`arch`) e já carrega o binário certo — `.dll` no Windows,
`.so` no Linux, `.dylib` no macOS. Se ele faltar, o programa não para no `import`:
o erro (com a lista de caminhos tentados) volta na primeira chamada que precisar da lib.
`nativeInfo()` mostra o que foi detectado: `{ platform, os, file, path, loaded, error }`.

| Plataforma | Arquivo | Neste repositório |
|---|---|---|
| `windows-x64` | `native/windows-x64/foguhdmi.dll` | incluído |
| `windows-arm64` | `native/windows-arm64/foguhdmi.dll` | — |
| `linux-x64` | `native/linux-x64/libfoguhdmi.so` | — |
| `linux-arm64` | `native/linux-arm64/libfoguhdmi.so` | — |
| `macos-x64` | `native/macos-x64/libfoguhdmi.dylib` | — |
| `macos-arm64` | `native/macos-arm64/libfoguhdmi.dylib` | — |

Ordem de busca: o caminho passado a `load(path)`, a variável `FOGU_HDMI_LIB`,
`native/<plataforma>/`, `native/` e o build do cargo no projeto em execução, uma
cópia em `fogu_modules/` e os pacotes instalados com `fogu add` (`$FOGU_HOME/fogu_modules`).

**Pré-requisitos**

- **Só Windows 10/11**: a lib usa DXGI/GDI (tela), WASAPI/Media Foundation (som) e SAPI (fala). Em Linux/macOS o `import` não falha, mas `load()` devolve um erro claro ("não funciona em linux-x64").
- HTML no monitor: Chrome ou Edge instalado (usado pelo `fogu:webview`).
- Build: Rust >= 1.85 (`fogu run build:native`).

**Gerar os binários**

```sh
fogu run build:native        # Windows: native/windows-<arch>/ (build.ps1)
sh build.sh                  # Linux/macOS/WSL: o sistema atual
sh build.sh all              # Linux x64/arm64 e macOS x64/arm64 (cargo-zigbuild + zig)
```

## Callbacks do fogu:ffi (Fogu 0.7+, testado com o 0.7.4)

A camada nativa usa `ffi.callback` (veja `_native.fg`, igual em todas as libs deste repositório):

- **Sink de saída**: a lib recebe um callback uma vez (`*_set_sink`) e, chamada com
  `buf = nil, cap = 0`, entrega a resposta inteira por ele. As funções antigas, com buffer
  do chamador, continuam exportadas e funcionando (API C compatível).
- **Regras do fogu:ffi respeitadas**: um callback só roda na thread do interpretador, durante
  uma chamada FFI — as threads da lib nunca chamam o Fogu; elas enfileiram e a chamada
  `*_dispatch`/leitura entrega na thread de quem chamou. Um erro dentro de um callback derruba o
  programa, então o callback só guarda os dados e os handlers do usuário rodam depois, fora
  do frame nativo (com `?`, erros e `sleep` normais).
- **Esperas cooperativas**: uma chamada FFI bloqueante congela todas as tarefas `fg`, então as
  esperas longas são feitas em fatias de 10 ms com `time.delay(1ms)` entre elas (o mesmo
  padrão do pacote oficial `hardware`).

`tela.fg` e `som.fg` dividem **uma** instância da lib (`_lib.fg`), porque o sink é global dentro da DLL. Saídas JSON (monitores, saídas de som, vozes) chegam pelo sink. `t.teclas()` devolve **todas** as teclas do quadro numa chamada (`fhd_keys`), sem perder teclas entre quadros; `som.esperar()` espera o fim na lib (`fhd_sound_wait`, acorda em ~2 ms) em fatias, sem `sleep(0.02)` e sem travar as tarefas `fg`.
- **Fogu 0.7.4**: o código compilado chama direto as funções FFI de assinatura numérica (sem
  alocação por chamada), então sondar status/eventos num laço é barato; `fogu .` roda o projeto
  (o script `start` do `fogu.json`); os tipos `int32`, `uint8`, `float64`... valem nas assinaturas.
<!-- /fogu:plataformas -->

## Build e execução

```sh
fogu install                 # dependência: lib hardware (HDMI-CEC)
fogu run build:native        # compila native/windows-x64/foguhdmi.dll (precisa de Rust >= 1.85)
fogu run main.fg             # Esc ou Q fecha; espaço adiciona bolas
fogu run main.fg --segundos 10 --foto out/tela.bmp
fogu test
cd foguhdmi && cargo test    # testes da lib Rust
```

Windows apenas (Win32/GDI, WASAPI, SAPI, Media Foundation). Se um programa
Fogu estiver usando a DLL durante o build, a antiga é renomeada para
`native/windows-<arch>/foguhdmi.dll.old-*` (e apagada num build seguinte).

## HDMI inativo ("Somente tela do PC")

Se o monitor estiver no cabo HDMI mas o Windows não o usar (Win+P →
"Somente tela do PC"), ele não tem área de trabalho e não dá para desenhar
nele. `tela.abrir()` detecta isso, muda para **Estender**, espera o monitor
aparecer e, em `t.fechar()`, volta ao modo anterior. Para não mexer na
configuração: `tela.abrir({ ativar: false })`.

## HTML no monitor HDMI

O Fogu não tem um motor de HTML próprio (`fogu:html` só lê e gera texto
HTML), então a página é renderizada por um Chrome/Edge controlado pelo
`fogu:webview`. Há dois modos ([html.fg](html.fg) mostra os dois):

```sh
fogu run html.fg               # HTML desenhado no canvas, misturado com desenho Fogu
fogu run html.fg --ao-vivo     # navegador em quiosque, tela cheia no HDMI
```

**No canvas**: `t.html(s)` renderiza o HTML num navegador sem janela, tira uma
foto e a desenha no canvas. Dá para combinar com as outras funções (desenhar
por cima, guardar como fundo, salvar). A primeira página leva ~0,5-1 s (abre o
navegador); as seguintes, ~50-150 ms. Bom para painéis que atualizam algumas
vezes por segundo; animações CSS/JS não aparecem, só o quadro capturado.

```fogu
const t = tela.abrir()?
t.html("<body style='background:#0f172a;color:white'><h1>Olá</h1></body>")?
t.circulo(100, 100, 40, 0xFF8800)           // desenho Fogu por cima
t.mostrar()?
t.pagina("https://example.com", { x: 0, y: 0, largura: 960, altura: 540 })?
```

**Ao vivo**: `tela.navegador()` abre o Chrome/Edge em modo quiosque no monitor
HDMI. HTML, CSS, animações, vídeo e JavaScript rodam no próprio navegador, na
velocidade dele, e o Fogu troca o conteúdo ou executa JS:

```fogu
const nav = tela.navegador({ html: "<h1>Olá</h1>" })?   // ou { url: "https://..." }
nav.html("<h1>outra página</h1>")?
nav.js("document.body.style.background = 'black'")?
nav.fechar()                                            // restaura o modo de projeção
```

| HTML | |
|---|---|
| `t.html(s, { x, y, largura, altura })` | renderiza HTML e desenha no canvas (padrão: tela toda) |
| `t.pagina(url, { x, y, largura, altura, espera })` | idem com uma URL; `espera`: seletor CSS a aguardar |
| `t.imagem(img \| caminho, x, y, largura = 0, altura = 0)` | desenha uma imagem do `fogu:image` ou um arquivo (png, jpg, webp…), com escala e alfa |
| `tela.htmlImagem(s, w, h) · paginaImagem(url, w, h)` | a renderização como imagem do `fogu:image` |
| `tela.fecharHtml()` | fecha o navegador sem janela |
| `tela.navegador({ monitor = "hdmi", url, html, ativar = true })` | Navegador · error |
| `nav.html(s) · ir(url) · js(codigo) · foto() · fechar()` | |

## Áudio e texto para fala no HDMI

O HDMI também leva o som: o Windows cria uma saída de áudio para o
monitor/TV (aqui, `LG FHD (HD Audio Driver for Display Audio)`). O
[som.fg](som.fg) toca nessa saída por padrão:

```sh
fogu run audio.fg                          # fala um texto e mostra a legenda no monitor
fogu run audio.fg --texto "Bom dia" --voz Helia --velocidade 1
fogu run audio.fg --arquivo musica.mp3     # depois da fala, toca o arquivo
fogu run audio.fg --sem-tela               # só o áudio
fogu run audio.fg --vozes                  # vozes instaladas e saídas de som
```

```fogu
import som from "./som.fg"

const f = som.falar("Olá! Isto sai pelo HDMI.", { voz: "pt" })?
f.esperar()?
const m = som.tocar("musica.mp3", { volume: 80 })?   // volta logo; toca em segundo plano
print(m.posicao(), "/", m.duracao(), "ms")
m.esperar()?
som.fechar()                                          // restaura o modo de projeção, se mudou
```

- **Texto para fala**: as vozes do Windows (SAPI e OneCore). Nesta máquina,
  em português, há a **Microsoft Helia (pt-PT)**; para pt-BR, instale a voz
  "Maria" ou "Francisca" em *Configurações → Hora e idioma → Fala*.
- **Arquivos**: wav, mp3, m4a/aac, wma, flac (via Media Foundation).
- **Saída**: o HDMI por padrão. Se ela não existir porque o Windows está em
  "Somente tela do PC", a tela é estendida para o HDMI (o áudio HDMI só existe
  com a imagem ativa) e `som.fechar()` desfaz. Outras saídas:
  `{ dispositivo: "padrao" }` ou parte do nome (`"Realtek"`).
- Vários sons podem tocar ao mesmo tempo (o Windows mistura).

| som.fg | |
|---|---|
| `som.falar(texto, { dispositivo = "hdmi", voz, velocidade = 0, volume = 100, ssml = false })` | Som · error. `voz`: parte do nome, idioma (`"pt"`) ou id; velocidade -10..10; volume 0..400 % |
| `som.gravarFala(texto, "out/fala.mp3", { voz, velocidade, ssml })` | grava a fala em .wav, .mp3 ou .m4a (pela extensão; codificadores do Windows) |
| `som.tocar(caminho, { dispositivo = "hdmi", volume = 100, repetir = false })` | Som · error |
| `som.tom(freq_hz, ms = 300, { dispositivo, volume })` | Som · error (bip senoidal) |
| `som.dispositivos()` | `[{ id, name, display, default }]` (`display`: áudio de monitor/TV) |
| `som.vozes()` | `[{ id, name, language }]` |
| `som.saidaHdmi()` | a saída de áudio HDMI ou nil |
| `som.volumeSistema({ dispositivo }) · setVolumeSistema(v, { dispositivo, mudo })` | volume/mudo do Windows da saída |
| `som.pico({ dispositivo })` | pico do sinal saindo agora pela saída (0.0..1.0) |
| `som.fechar()` | desfaz a ativação do HDMI feita pelo som |

**Não sai som?** `fogu run audio.fg --diagnostico` mostra o volume/mudo de
cada saída, toca um tom no HDMI e mede o sinal que passa por ela. Se o sinal
chega à saída HDMI e mesmo assim não se ouve nada, o problema está no monitor:
muitos monitores (como os LG FHD) não têm colunas e só levam o som à saída de
fones; veja também o volume/mudo no menu do monitor. Para usar outra saída:
`fogu run audio.fg --saida padrao` (ou parte do nome, como `Realtek`).
| `Som`: `tocando() · posicao() · duracao() · volume(v) · esperar(maximo = 0) · parar()` | tempos em ms |

Com `ssml: true` o texto aceita a marcação do SAPI, por exemplo
`<rate speed="3">rápido</rate>`, `<silence msec="500"/>`, `<emph>ênfase</emph>`.

## HDMI-CEC (lib `hardware`)

O `main.fg` também tenta ligar a TV e trocá-la para a nossa entrada com
`hardware/hdmi` (HDMI-CEC). Placas de vídeo de PC não têm CEC: no Windows é
preciso um adaptador USB-CEC da Pulse-Eight com o `cec-client` (libCEC) no
`PATH`. Sem ele, o programa avisa e segue desenhando normalmente.

## API

```fogu
import tela from "./tela.fg"
import time from "fogu:time"

const t = tela.abrir()?                       // tela cheia no monitor HDMI
t.limpar(0x101830)
t.degrade(0, 0, t.largura, t.altura, 0x0B1026, 0xF27D52)
t.circulo(t.largura / 2, t.altura / 2, 200, 0xFF8800)
t.texto(40, 40, "Olá, HDMI!", 0xFFFFFF, 64)
t.mostrar()?                                  // envia o quadro para o monitor
while t.aberta() { time.sleep(0.05) }         // Esc fecha
t.fechar()?
```

| módulo | |
|---|---|
| `tela.abrir({ monitor = "hdmi" \| índice, largura, altura, titulo, ativar = true })` | Tela · error. Sem largura/altura: tela cheia |
| `tela.canvas(largura, altura)` | Tela fora da tela (imagens, testes) |
| `tela.monitores()` | `[{ index, name, device, output, hdmi, primary, x, y, width, height }]` (monitores ativos) |
| `tela.conectados()` | `[{ id, name, output, hdmi, active }]` (inclui os inativos) |
| `tela.monitorHdmi()` | o primeiro monitor HDMI ativo ou nil |
| `tela.topologia() · setTopologia("interna" \| "duplicar" \| "estender" \| "externa") · ativarHdmi()` | modo de projeção (Win+P) |
| `tela.rgb(r, g, b) · rgba(r, g, b, a) · channels(c) · hex("#ff8800") · hsv(h, s, v) · mix(c1, c2, t)` | cores (`channels` devolve `[r, g, b]`) |
| `tela.sin · cos · sqrt · PI · random(n) · horaLocal()` | matemática (o `fogu:math` não tem trigonometria) |
| `tela.teclas` | códigos de tecla para `t.tecla()` |

| `Tela` | |
|---|---|
| `largura · altura · monitor` | campos |
| `limpar(cor) · ponto(x, y, cor)` | |
| `retangulo(x, y, w, h, cor) · moldura(x, y, w, h, cor, espessura) · arredondado(x, y, w, h, raio, cor)` | |
| `linha(x0, y0, x1, y1, cor, espessura) · circulo(cx, cy, r, cor) · anel(cx, cy, r, cor, espessura)` | |
| `triangulo(x0, y0, x1, y1, x2, y2, cor) · degrade(x, y, w, h, c1, c2, vertical = true)` | |
| `fonte(nome, negrito) · texto(x, y, s, cor, tamanho) · textoCentro(cx, y, s, cor, tamanho) · medir(s, tamanho)` | texto GDI |
| `guardarFundo() · fundo()` | guarda o desenho atual e o redesenha a cada quadro |
| `mostrar()` | envia o quadro para o monitor |
| `aberta() · tecla() · mouse()` | `mouse()` → `{ x, y, botoes }` |
| `salvar("out/x.bmp") · fechar()` | `fechar()` restaura o modo de projeção, se mudou |

Cores: `0xRRGGBB` (opaca) ou `tela.rgba(...)` com transparência. As coordenadas
podem ser floats (são arredondadas).

## Como funciona

A lib Rust roda a janela numa thread própria com o loop de mensagens do
Windows (um callback do `fogu:ffi` só pode rodar na thread do interpretador,
então essa thread enfileira as teclas e o Fogu as lê com `t.teclas()`). O Fogu desenha num canvas em memória (uma DIB section
32 bpp: as formas são rasterizadas em Rust com anti-aliasing, o texto usa o
GDI) e `mostrar()` copia o quadro para a janela. Os monitores e o tipo de
conexão de cada um (HDMI, DisplayPort, interno...) vêm do `QueryDisplayConfig`.
Adaptadores USB-C → HDMI costumam aparecer como `displayport`; nesse caso use
`tela.abrir({ monitor: índice })`.
