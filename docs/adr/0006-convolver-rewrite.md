# 0006. 컨볼버는 MultiConvolution을 Rust로 새로 쓴다

상태: 채택, 2026-10-11

## 배경

EqualizerAPO-XT의 MultiConvolution(115dkk 작성)은 여러 입력을 귀별로 합성곱해 더하는 일을 이미 효율적으로 합니다. 하지만 C++이고 libHybridConv, FFTW, IrCache를 끌고 옵니다.

## 결정

MultiConvolution을 `crates/convolver`에 Rust와 MIT로 새로 씁니다(Copyright 2026 115dkk). FFT는 realfft를 쓰고, libHybridConv, FFTW, IrCache는 들이지 않습니다. 균일 분할로 시작하고, 긴 꼬리를 위한 비균일 분할은 같은 타입 안에 나중에 넣습니다. 어댑터가 하나뿐이므로 트레이트는 만들지 않습니다.

## 결과

- 입력마다 정방향 FFT 한 번, 귀마다 주파수 영역 곱셈 누적, 블록당 역FFT 두 번이라는 구조를 그대로 가져옵니다.
- 분할 크기는 도착 블록의 약수로 맞춰 추가 지연을 만들지 않습니다(게임의 MAT은 480프레임 반 버스트).
- 짧은 IR의 직접 합성곱, XT의 출력과 대조하는 테스트를 둡니다.
