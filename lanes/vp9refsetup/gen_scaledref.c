#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <vpx/vpx_encoder.h>
#include <vpx/vp8cx.h>
static FILE *O;
static void drain(vpx_codec_ctx_t *c){
  const vpx_codec_cx_pkt_t *pkt; vpx_codec_iter_t it=NULL;
  while((pkt=vpx_codec_get_cx_data(c,&it))) if(pkt->kind==VPX_CODEC_CX_FRAME_PKT){
    unsigned char sz[4]; uint32_t n=pkt->data.frame.sz; memcpy(sz,&n,4); fwrite(sz,1,4,O);
    unsigned long ts=(unsigned long)pkt->data.frame.pts; unsigned char t[8]; memcpy(t,&ts,8); fwrite(t,1,8,O);
    fwrite(pkt->data.frame.buf,1,n,O);
  }
}
static void fill(vpx_image_t*img,int f){
  int W=img->d_w,H=img->d_h;
  for(int y=0;y<H;y++)for(int x=0;x<W;x++){
    int v=(x*160/W)+(y*120/H)+f*4;
    img->planes[0][y*img->stride[0]+x]=(unsigned char)(v&0xff);}
  // A moving 200x120 high-contrast box (nonzero MVs + residual under scaling).
  int bx=(f*37)%(W-200), by=(f*23)%(H-120);
  for(int y=by;y<by+120;y++)for(int x=bx;x<bx+200;x++)
    img->planes[0][y*img->stride[0]+x]=(((x/16+y/16)&1)?230:25);
  for(int p=1;p<3;p++){int cw=(W+1)/2,ch=(H+1)/2;
    for(int y=0;y<ch;y++)for(int x=0;x<cw;x++){
      int v=128+(x*40/cw)-(y*30/ch)+f*(p?2:-2);
      img->planes[p][y*img->stride[p]+x]=(unsigned char)(v&0xff);}}
}
int main(int argc,char**argv){
  const char*out=argv[1]; int W=atoi(argv[2]),H=atoi(argv[3]),N=atoi(argv[4]),br=atoi(argv[5]);
  vpx_codec_enc_cfg_t cfg; vpx_codec_enc_config_default(vpx_codec_vp9_cx(),&cfg,0);
  cfg.g_w=W;cfg.g_h=H;cfg.g_timebase.num=1;cfg.g_timebase.den=30;
  cfg.rc_end_usage=VPX_CBR;cfg.rc_target_bitrate=br;cfg.g_lag_in_frames=0;
  cfg.rc_resize_allowed=1; cfg.rc_buf_sz=200; cfg.rc_buf_initial_sz=100; cfg.rc_buf_optimal_sz=150;
  cfg.rc_min_quantizer=4; cfg.rc_max_quantizer=56;
  cfg.kf_min_dist=9999;cfg.kf_max_dist=9999;
  vpx_codec_ctx_t c;
  if(vpx_codec_enc_init(&c,vpx_codec_vp9_cx(),&cfg,0)){fprintf(stderr,"init %s\n",vpx_codec_error(&c));return 1;}
  vpx_codec_control(&c,VP8E_SET_CPUUSED,5);
  O=fopen(out,"wb"); unsigned char hdr[32];memset(hdr,0,32);memcpy(hdr,"DKIF",4);hdr[6]=32;memcpy(hdr+8,"VP90",4);
  hdr[12]=W&0xff;hdr[13]=W>>8;hdr[14]=H&0xff;hdr[15]=H>>8;hdr[16]=30;hdr[17]=1;hdr[20]=1;
  uint32_t nf=N;memcpy(hdr+24,&nf,4);fwrite(hdr,1,32,O);
  vpx_image_t*img=vpx_img_alloc(NULL,VPX_IMG_FMT_I420,W,H,1);
  for(int i=0;i<N;i++){ fill(img,i); if(vpx_codec_encode(&c,img,i,1,0,VPX_DL_GOOD_QUALITY)){fprintf(stderr,"enc %d %s\n",i,vpx_codec_error(&c));return 1;} drain(&c); }
  for(;;){vpx_codec_encode(&c,NULL,N,1,0,VPX_DL_GOOD_QUALITY);int b=ftell(O);drain(&c);if(ftell(O)==b)break;}
  fclose(O);vpx_codec_destroy(&c);fprintf(stderr,"wrote %s %dx%d n=%d br=%d\n",out,W,H,N,br);return 0;
}
